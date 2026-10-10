//! The TMDB watch-providers read
//! (<https://developer.themoviedb.org/reference/movie-watch-providers>,
//! <https://developer.themoviedb.org/reference/tv-series-watch-providers>).
//!
//! TMDB takes a v3 API key as the `api_key` query parameter or a v4 read
//! access token (a JWT, so it holds dots) as `Authorization: Bearer`
//! (<https://developer.themoviedb.org/docs/authentication-application>).
//! Because the v3 key travels in the URL, no error here carries one.

use super::{parse_flatrate, Looked, Provider, StreamingCache, Title};

/// TMDB's API host.
pub const TMDB_BASE: &str = "https://api.themoviedb.org";

/// Why a lookup failed. None of them holds the URL (it may hold the key).
#[derive(Debug, thiserror::Error)]
pub enum TmdbError {
    #[error("no answer: {0}")]
    Transport(reqwest::Error),
    #[error("HTTP {0}")]
    Status(u16),
    #[error("{0}")]
    Body(crate::body::BodyError),
    #[error("unexpected answer")]
    Parse,
}

/// The flatrate providers of `region` for one title.
pub async fn flatrate(http: &reqwest::Client, base: &str, key: &str, title: Title, region: &str) -> Result<Vec<Provider>, TmdbError> {
    let request = http.get(format!("{}{}", base.trim_end_matches('/'), title.path()));
    let request = if key.contains('.') { request.bearer_auth(key) } else { request.query(&[("api_key", key)]) };
    let response = request.send().await.map_err(|error| TmdbError::Transport(error.without_url()))?;
    let status = response.status();
    // A redirect is not followed (the key would travel with it) and reads as a failure.
    if !status.is_success() {
        return Err(TmdbError::Status(status.as_u16()));
    }
    let body = crate::body::read(response).await.map_err(TmdbError::Body)?;
    let answer: serde_json::Value = serde_json::from_slice(&body).map_err(|_| TmdbError::Parse)?;
    parse_flatrate(&answer, region).ok_or(TmdbError::Parse)
}

/// One cycle's lookups.
#[derive(Debug)]
pub struct Refreshed {
    pub looked_up: usize,
    /// The failure that ended the cycle's lookups early, if any.
    pub problem: Option<TmdbError>,
}

/// Look up the cycle's due titles ([`StreamingCache::due`]) into `cache`,
/// forgetting titles no longer in `titles`. A 404 (TMDB does not know the id)
/// is stored as streaming nowhere, so it is not asked again every cycle; any
/// other failure stops the cycle's lookups and leaves earlier ones standing.
pub async fn refresh(
    http: &reqwest::Client,
    base: &str,
    key: &str,
    region: &str,
    cache: &mut StreamingCache,
    titles: &[Title],
    now: u64,
) -> Refreshed {
    // An empty list is a failed inventory more often than an empty library.
    if !titles.is_empty() {
        cache.retain(titles);
    }
    let mut refreshed = Refreshed { looked_up: 0, problem: None };
    for title in cache.due(titles, region, now) {
        let flatrate = match flatrate(http, base, key, title, region).await {
            Ok(flatrate) => flatrate,
            Err(TmdbError::Status(404)) => Vec::new(),
            Err(error) => {
                refreshed.problem = Some(error);
                break;
            }
        };
        cache.insert(title, Looked { looked_at: now, region: region.to_owned(), flatrate });
        refreshed.looked_up += 1;
    }
    refreshed
}
