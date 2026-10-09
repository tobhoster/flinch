//! Posters for the embedding refresh, when the operator switched them on.
//!
//! Each title's poster is its *arr's upstream artwork (`images` → `poster` →
//! `remoteUrl`, a TMDB or TheTVDB CDN address): fetched anonymously, with no
//! *arr or Plex credential and no cookie, capped in size, decoded, and run
//! through the vision tower. The arr-local copy is never used: reaching it
//! would mean sending an API key.
//!
//! A poster never makes the refresh fail. One the CDN refuses (4xx), one too
//! large or one that does not decode leaves the title described by its text
//! alone until its poster URL changes; a network error or a CDN fault (5xx,
//! 429) leaves the title waiting for a later cycle.

use flinch_archive::arr::ArrImage;
use flinch_archive::embedding::{self, ImageTokens, ModelError, VisionEncoder};
use std::time::Duration;
use tokio::runtime::Handle;

/// The largest poster fetched. A TMDB original is 1–3 MB.
const MAX_POSTER_BYTES: usize = 8 << 20;

/// The upstream poster's address, if the *arr lists one.
pub(super) fn poster_url(images: &[ArrImage]) -> Option<String> {
    images.iter().find(|image| image.cover_type == "poster").and_then(|image| image.remote_url.clone()).filter(|url| !url.is_empty())
}

/// Why a poster was not used.
enum Failure {
    /// Try again next cycle.
    Later(String),
    /// Describe the title by text alone until its poster changes.
    Unusable(String),
}

/// Fetches and encodes posters from the embedding worker's blocking thread.
pub(super) struct Posters<'a> {
    vision: &'a VisionEncoder,
    http: reqwest::Client,
    runtime: Handle,
    unusable: usize,
    deferred: usize,
    last: Option<String>,
}

impl<'a> Posters<'a> {
    pub(super) fn new(vision: &'a VisionEncoder, runtime: Handle) -> Result<Self, reqwest::Error> {
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::limited(3))
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .build()?;
        Ok(Self { vision, http, runtime, unusable: 0, deferred: 0, last: None })
    }

    /// What to embed for a title whose description is `text`: with its
    /// poster's soft tokens when there is a usable one, else the text alone.
    /// `None` when the poster could not be fetched now; the title waits.
    pub(super) fn describe(&mut self, text: &str, url: Option<&str>) -> Result<Option<(String, Option<ImageTokens>)>, ModelError> {
        let Some(url) = url else { return Ok(Some((text.to_string(), None))) };
        let failure = match self.runtime.block_on(fetch(&self.http, url)) {
            Ok(bytes) => match self.vision.preprocessing().patches_of_file(&bytes) {
                Ok(patches) => return Ok(Some((embedding::with_poster(text), Some(self.vision.encode(&patches)?)))),
                Err(error) => Failure::Unusable(format!("{url}: {error}")),
            },
            Err(failure) => failure,
        };
        match failure {
            Failure::Later(why) => {
                self.deferred += 1;
                self.last = Some(why);
                Ok(None)
            }
            Failure::Unusable(why) => {
                self.unusable += 1;
                self.last = Some(why);
                Ok(Some((text.to_string(), None)))
            }
        }
    }

    /// What went wrong with posters this cycle, in words.
    pub(super) fn problem(&self) -> Option<String> {
        let last = self.last.as_deref()?;
        Some(format!(
            "posters: {} unusable (those titles are described by text alone), {} to retry next cycle; last: {last}",
            self.unusable, self.deferred
        ))
    }
}

/// GET a poster, anonymously and at most [`MAX_POSTER_BYTES`].
async fn fetch(http: &reqwest::Client, url: &str) -> Result<Vec<u8>, Failure> {
    let parsed = reqwest::Url::parse(url).map_err(|error| Failure::Unusable(format!("{url}: {error}")))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(Failure::Unusable(format!("{url}: not an http(s) address")));
    }
    let later = |error: reqwest::Error| Failure::Later(format!("{url}: {error}"));
    let mut response = http.get(parsed).send().await.map_err(later)?;
    let status = response.status();
    if status.is_server_error() || status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        return Err(Failure::Later(format!("{url}: HTTP {}", status.as_u16())));
    }
    if !status.is_success() {
        return Err(Failure::Unusable(format!("{url}: HTTP {}", status.as_u16())));
    }
    let too_large = || Failure::Unusable(format!("{url}: larger than {} MB", MAX_POSTER_BYTES >> 20));
    if response.content_length().is_some_and(|length| length > MAX_POSTER_BYTES as u64) {
        return Err(too_large());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(later)? {
        if bytes.len() + chunk.len() > MAX_POSTER_BYTES {
            return Err(too_large());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}
