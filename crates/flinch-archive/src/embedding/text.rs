//! The text each subject is embedded from.
//!
//! Content metadata only: what the title is, never what this household did
//! with it. The taste classifier is fitted on past plays, so a play count, a
//! rating, an added date, a keep label or a request in the text would hand it
//! the outcome it is meant to predict. Each recipe input is read here by name;
//! nothing else of an *arr or Plex row reaches the text.
//!
//! The text is deterministic: lists are deduplicated and sorted, whitespace is
//! collapsed, so its hash changes only when the description does.

use crate::arr::{ArrMovie, ArrSeries};
use crate::plex::{PlexMetadata, PlexTag};

/// Bumped whenever the recipe changes, which invalidates every cached vector.
pub const RECIPE_VERSION: u32 = 1;
/// The recipe of text-and-poster vectors: the text recipe plus a poster
/// placeholder, a space of its own (see [`super::weights::poster_model_id`]).
pub const POSTER_RECIPE_VERSION: u32 = 2;
/// Where EmbeddingGemma 2 takes an image's soft tokens.
pub const IMAGE_PLACEHOLDER: &str = "<|image|>";
/// The line holding the poster, right after the title so a cut overview never
/// takes it.
const POSTER_LINE: &str = "\nPoster: <|image|>";

/// EmbeddingGemma 2's prompt for symmetric classification.
const PREFIX: &str = "task: classification | query: ";
/// The longest text sent, in characters. The overview comes last, so a long
/// one is what gets cut; the model's context is far larger, this bounds cost.
const MAX_CHARS: usize = 2_000;

/// The text of a subject embedded with its poster: its description with the
/// poster's placeholder after the title line. A placeholder the metadata
/// itself happens to hold is dropped, so the poster is the only image.
pub fn with_poster(text: &str) -> String {
    let text = text.replace(IMAGE_PLACEHOLDER, "");
    let at = text.find('\n').unwrap_or(text.len());
    format!("{}{POSTER_LINE}{}", &text[..at], &text[at..])
}

/// The cache key of a text embedded with the poster at `url`: a new poster
/// is a new description.
pub fn poster_hash(text: &str, url: &str) -> String {
    text_hash(&format!("{text}\n{url}"))
}

/// The text of a Radarr movie, with its Plex row when Plex matched it.
pub fn movie_text(movie: &ArrMovie, plex: Option<&PlexMetadata>) -> String {
    let mut text = Text::new(&movie.title);
    text.line("Year", movie.year.filter(|year| *year > 0).map(|year| year.to_string()));
    text.line("Kind", Some("movie".to_string()));
    text.list("Genres", movie.genres.iter().map(String::as_str).chain(tags(plex.map_or(&[][..], |row| &row.genres))), 8);
    text.line("Certification", first(&movie.certification, plex.and_then(|row| row.content_rating.as_ref())));
    text.line("Runtime", movie.runtime.filter(|minutes| *minutes > 0).map(|minutes| format!("{minutes} min")));
    text.line("Original language", movie.original_language.as_ref().map(|language| language.name.clone()));
    text.line("Studio", first(&movie.studio, plex.and_then(|row| row.studio.as_ref())));
    text.line("Collection", movie.collection.as_ref().map(|collection| collection.title.clone()));
    plex_credits(&mut text, plex);
    text.line("Overview", first(&movie.overview, plex.and_then(|row| row.summary.as_ref())));
    text.finish()
}

/// The text of a Sonarr series (every season shares it), with the show's Plex
/// row when Plex matched one of its seasons.
pub fn series_text(series: &ArrSeries, plex: Option<&PlexMetadata>) -> String {
    let mut text = Text::new(&series.title);
    text.line("Year", series.year.filter(|year| *year > 0).map(|year| year.to_string()));
    text.line("Kind", Some("series".to_string()));
    text.line("Series type", Some(series.series_type.clone()));
    text.list("Genres", series.genres.iter().map(String::as_str).chain(tags(plex.map_or(&[][..], |row| &row.genres))), 8);
    text.line("Certification", first(&series.certification, plex.and_then(|row| row.content_rating.as_ref())));
    text.line("Runtime", series.runtime.filter(|minutes| *minutes > 0).map(|minutes| format!("{minutes} min per episode")));
    text.line("Original language", series.original_language.as_ref().map(|language| language.name.clone()));
    text.line("Network", first(&series.network, plex.and_then(|row| row.studio.as_ref())));
    plex_credits(&mut text, plex);
    text.line("Overview", first(&series.overview, plex.and_then(|row| row.summary.as_ref())));
    text.finish()
}

/// The people and places only Plex lists, and its tagline.
fn plex_credits(text: &mut Text, plex: Option<&PlexMetadata>) {
    let Some(row) = plex else { return };
    text.list("Country", tags(&row.countries), 4);
    text.list("Directed by", tags(&row.directors), 4);
    text.list("Written by", tags(&row.writers), 4);
    text.list("Starring", tags(&row.roles), 8);
    text.line("Tagline", row.tagline.clone());
}

fn tags(tags: &[PlexTag]) -> impl Iterator<Item = &str> {
    tags.iter().map(|tag| tag.tag.as_str())
}

/// The *arr's value, else Plex's.
fn first(arr: &Option<String>, plex: Option<&String>) -> Option<String> {
    [arr.as_ref(), plex].into_iter().flatten().map(|value| collapse(value)).find(|value| !value.is_empty())
}

/// Whitespace runs as one space, trimmed.
fn collapse(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

struct Text(String);

impl Text {
    fn new(title: &str) -> Self {
        Self(format!("{PREFIX}Title: {}", collapse(title)))
    }

    fn line(&mut self, label: &str, value: Option<String>) {
        if let Some(value) = value.map(|value| collapse(&value)).filter(|value| !value.is_empty()) {
            self.0.push_str(&format!("\n{label}: {value}"));
        }
    }

    /// At most `cap` distinct entries, case-insensitively, in sorted order:
    /// the order a server happens to list them in never changes the text.
    fn list<'a>(&mut self, label: &str, values: impl Iterator<Item = &'a str>, cap: usize) {
        let mut values: Vec<String> = values.map(collapse).filter(|value| !value.is_empty()).collect();
        values.sort_by(|a, b| a.to_lowercase().cmp(&b.to_lowercase()).then_with(|| a.cmp(b)));
        values.dedup_by(|a, b| a.to_lowercase() == b.to_lowercase());
        values.truncate(cap);
        self.line(label, (!values.is_empty()).then(|| values.join(", ")));
    }

    fn finish(self) -> String {
        match self.0.char_indices().nth(MAX_CHARS) {
            Some((cut, _)) => self.0[..cut].to_string(),
            None => self.0,
        }
    }
}

/// A stable fingerprint of a text (64-bit FNV-1a, hex): the cache's test for
/// "this subject's description changed". Std's hashers may change between
/// releases; this one is fixed, so an upgrade does not re-embed the library.
pub fn text_hash(text: &str) -> String {
    let hash = text.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3));
    format!("{hash:016x}")
}

/// Content metadata is optional colour for the text: a value of an unexpected
/// shape reads as absent, never failing the *arr or Plex row it came in.
pub(crate) fn tolerant<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Default + serde::de::DeserializeOwned,
{
    let value = <serde_json::Value as serde::Deserialize>::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).unwrap_or_default())
}
