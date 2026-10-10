//! Getting EmbeddingGemma 2's weights onto the state volume, once.
//!
//! The published checkpoint is one 1.5 GB safetensors file holding the text,
//! vision and audio towers. FLINCH reads its header and asks for exactly the
//! byte ranges of the towers it runs, writing a safetensors file of its own
//! per tower: the text tensors (one 542 MB block) always, the vision tower and
//! its projection into the text model (335 MB in two blocks) only when posters
//! are switched on. The audio tower is never fetched. The revision is pinned:
//! the byte offsets, the tokenizer and every cached vector belong to it, and a
//! new revision is a deliberate change here, not a silent drift upstream.
//!
//! Nothing secret is sent (the model is Apache 2.0 and ungated), so unlike
//! every other client FLINCH has, this one follows Hugging Face's redirects to
//! its file CDN.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const REPO: &str = "google/embeddinggemma-2";
pub const REVISION: &str = "914f7f89142e33e77833254d9c9b90c3cef7303b";
/// The text model.
pub const TEXT_TENSORS: &[&str] = &["language_model."];
/// The vision tower, and the projection of its soft tokens into the text model.
pub const VISION_TENSORS: &[&str] = &["vision_tower.", "embed_vision."];
/// A header larger than this is not a safetensors header.
const MAX_HEADER: u64 = 64 << 20;

/// `google/embeddinggemma-2@914f7f89`: what the vector cache is keyed by.
pub fn model_id() -> String {
    format!("{REPO}@{}", &REVISION[..8])
}

/// The cache key of text-and-poster vectors: their own space, so switching
/// posters on or off re-embeds every title instead of mixing the two.
pub fn poster_model_id() -> String {
    format!("{}+posters", model_id())
}

#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("model download failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("model download of {file}: HTTP {status}")]
    Status { file: &'static str, status: u16 },
    #[error("model weights header unusable: {0}")]
    Header(String),
    #[error("model download wrote {written} of {wanted} bytes")]
    Short { written: u64, wanted: u64 },
    #[error("model files: {0}")]
    Io(#[from] std::io::Error),
}

/// Where one revision's files live.
#[derive(Debug, Clone)]
pub struct ModelFiles {
    dir: PathBuf,
}

impl ModelFiles {
    pub fn in_state(state_dir: &Path) -> Self {
        Self { dir: state_dir.join("models").join(format!("embeddinggemma-2-{}", &REVISION[..8])) }
    }

    pub fn tokenizer(&self) -> PathBuf {
        self.dir.join("tokenizer.json")
    }

    /// The text-only safetensors file FLINCH writes.
    pub fn weights(&self) -> PathBuf {
        self.dir.join("text.safetensors")
    }

    /// The vision-only safetensors file FLINCH writes.
    pub fn vision_weights(&self) -> PathBuf {
        self.dir.join("vision.safetensors")
    }

    /// The processor settings, whose `image_processor` block says how an image
    /// becomes patches.
    pub fn processor_config(&self) -> PathBuf {
        self.dir.join("processor_config.json")
    }

    /// The text files are in place (each is renamed into place only when whole).
    pub fn present(&self) -> bool {
        self.tokenizer().is_file() && self.weights().is_file()
    }

    /// The vision files are in place too.
    pub fn vision_present(&self) -> bool {
        self.present() && self.processor_config().is_file() && self.vision_weights().is_file()
    }

    /// Download whatever of the text model is missing.
    pub async fn fetch(&self) -> Result<(), FetchError> {
        let http = self.client()?;
        if !self.tokenizer().is_file() {
            let body = get(&http, "tokenizer.json", None).await?.bytes().await?;
            crate::persist::replace(&self.tokenizer(), &body)?;
        }
        if !self.weights().is_file() {
            self.fetch_tensors(&http, TEXT_TENSORS, &self.weights()).await?;
        }
        Ok(())
    }

    /// Download whatever of the text model and the vision tower is missing.
    pub async fn fetch_vision(&self) -> Result<(), FetchError> {
        self.fetch().await?;
        let http = self.client()?;
        if !self.processor_config().is_file() {
            let body = get(&http, "processor_config.json", None).await?.bytes().await?;
            crate::persist::replace(&self.processor_config(), &body)?;
        }
        if !self.vision_weights().is_file() {
            self.fetch_tensors(&http, VISION_TENSORS, &self.vision_weights()).await?;
        }
        Ok(())
    }

    fn client(&self) -> Result<reqwest::Client, FetchError> {
        std::fs::create_dir_all(&self.dir)?;
        Ok(reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::limited(5))
            .connect_timeout(Duration::from_secs(30))
            .timeout(Duration::from_secs(60 * 60))
            .build()?)
    }

    /// Write the tensors under `prefixes` to `target`: their header, then each
    /// of their byte ranges of the checkpoint in order.
    async fn fetch_tensors(&self, http: &reqwest::Client, prefixes: &[&str], target: &Path) -> Result<(), FetchError> {
        let length = get(http, "model.safetensors", Some((0, 7))).await?.bytes().await?;
        let length = u64::from_le_bytes(length.as_ref().try_into().map_err(|_| FetchError::Header("no length prefix".into()))?);
        if length > MAX_HEADER {
            return Err(FetchError::Header(format!("{length}-byte header")));
        }
        let header = get(http, "model.safetensors", Some((8, 7 + length))).await?.bytes().await?;
        let subset = subset(&header, prefixes)?;

        let mut temp_name = target.as_os_str().to_owned();
        temp_name.push(".part");
        let temp = PathBuf::from(temp_name);
        let mut file = std::fs::File::create(&temp)?;
        file.write_all(&(subset.header.len() as u64).to_le_bytes())?;
        file.write_all(&subset.header)?;
        let data = 8 + length;
        for &(start, end) in &subset.ranges {
            let mut response = get(http, "model.safetensors", Some((data + start, data + end - 1))).await?;
            let mut written = 0u64;
            while let Some(chunk) = response.chunk().await? {
                file.write_all(&chunk)?;
                written += chunk.len() as u64;
            }
            if written != end - start {
                return Err(FetchError::Short { written, wanted: end - start });
            }
        }
        file.sync_all()?;
        std::fs::rename(&temp, target)?;
        Ok(())
    }
}

/// GET one file of the pinned revision, optionally a byte range (inclusive).
async fn get(http: &reqwest::Client, file: &'static str, range: Option<(u64, u64)>) -> Result<reqwest::Response, FetchError> {
    let mut request = http.get(format!("https://huggingface.co/{REPO}/resolve/{REVISION}/{file}"));
    if let Some((first, last)) = range {
        request = request.header(reqwest::header::RANGE, format!("bytes={first}-{last}"));
    }
    let response = request.send().await?;
    let status = response.status();
    // A range answered whole would download the 1.5 GB file: refuse it.
    let wanted = if range.is_some() { reqwest::StatusCode::PARTIAL_CONTENT } else { reqwest::StatusCode::OK };
    if status != wanted {
        return Err(FetchError::Status { file, status: status.as_u16() });
    }
    Ok(response)
}

/// Some of a checkpoint's tensors, as a safetensors file of their own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subset {
    /// The new file's JSON header, space-padded to a multiple of 8 bytes.
    pub header: Vec<u8>,
    /// The tensors' data in the checkpoint's data section, as maximal
    /// contiguous half-open byte ranges in file order: concatenated, they are
    /// the new file's data section.
    pub ranges: Vec<(u64, u64)>,
}

/// The tensors whose names start with one of `prefixes`: their header with
/// offsets into the concatenation of their byte ranges, and those ranges.
pub fn subset(header: &[u8], prefixes: &[&str]) -> Result<Subset, FetchError> {
    let parsed: serde_json::Map<String, serde_json::Value> =
        serde_json::from_slice(header).map_err(|error| FetchError::Header(error.to_string()))?;
    let mut tensors: Vec<(String, serde_json::Value, u64, u64)> = Vec::new();
    for (name, entry) in parsed.into_iter().filter(|(name, _)| prefixes.iter().any(|prefix| name.starts_with(prefix))) {
        let offsets = entry
            .get("data_offsets")
            .and_then(|offsets| offsets.as_array())
            .map(|offsets| offsets.iter().filter_map(serde_json::Value::as_u64).collect::<Vec<_>>());
        let Some([from, to]) = offsets.as_deref().and_then(|offsets| <[u64; 2]>::try_from(offsets).ok()).filter(|[from, to]| from <= to)
        else {
            return Err(FetchError::Header(format!("{name} has no data offsets")));
        };
        tensors.push((name, entry, from, to));
    }
    if tensors.is_empty() {
        return Err(FetchError::Header(format!("no tensors named {}…", prefixes.join("…, "))));
    }
    tensors.sort_by_key(|tensor| (tensor.2, tensor.3));
    let mut ranges: Vec<(u64, u64)> = Vec::new();
    let mut kept = serde_json::Map::new();
    let mut written = 0u64;
    for (name, mut entry, from, to) in tensors {
        match ranges.last_mut() {
            Some((_, end)) if from < *end => return Err(FetchError::Header(format!("{name} overlaps another tensor"))),
            Some((_, end)) if from == *end => *end = to,
            _ => ranges.push((from, to)),
        }
        entry["data_offsets"] = serde_json::json!([written, written + (to - from)]);
        written += to - from;
        kept.insert(name, entry);
    }
    let mut header = serde_json::to_vec(&kept).map_err(|error| FetchError::Header(error.to_string()))?;
    // The data that follows must start 8-byte aligned; the format pads with spaces.
    header.resize(header.len().div_ceil(8) * 8, b' ');
    Ok(Subset { header, ranges })
}
