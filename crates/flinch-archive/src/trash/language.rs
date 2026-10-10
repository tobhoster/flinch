//! A language preference as one switch: TRaSH-Guides' `Language: Not …`
//! custom formats laid over every synced profile of an instance.
//!
//! Why: the guide profiles leave language to the operator, and the usual
//! wish ("English, but take another language when nothing else exists") is
//! two scores and a minimum that must agree. The formats match a release
//! *without* the wanted language, so:
//!
//! - strict: the format scores -10000, the guide's own reject score
//!   (`trash_scores.default` of each format at [`super::GUIDE_COMMIT`]);
//! - with fallback: it scores `-fallback_penalty` and the profile's minimum
//!   score drops by the same amount. Every release the profile took before
//!   is still taken, one in the wanted language ranks that much higher, and
//!   the guide's -10000 unwanted formats stay rejected because the penalty
//!   stays below 10000.
//!
//! Radarr profiles also get language `Any`, as TRaSH advises with language
//! formats: a profile language of `Original` would refuse the fallback.

use super::config::InvalidTrashConfig;
use crate::capacity::App;
use serde::{Deserialize, Serialize};

/// The guide's reject score for these formats.
pub const REJECT: i32 = -10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Preferred {
    English,
    /// The title's original language.
    Original,
    French,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LanguagePreset {
    pub prefer: Preferred,
    /// Take another language when the preferred one is not to be had.
    #[serde(default)]
    pub fallback: bool,
    /// How far a release in another language ranks below; 1 to 9999.
    #[serde(default = "default_penalty")]
    pub fallback_penalty: i32,
}

fn default_penalty() -> i32 {
    1000
}

impl LanguagePreset {
    pub fn validate(&self) -> Result<(), InvalidTrashConfig> {
        if !(1..REJECT.abs()).contains(&self.fallback_penalty) {
            return Err(InvalidTrashConfig("the language fallback_penalty must be 1 to 9999, below the guide's reject score"));
        }
        Ok(())
    }

    /// The format, its score, and how far the profile's minimum moves.
    pub fn rule(&self, app: App) -> LanguageRule {
        let (score, min_shift) = if self.fallback { (-self.fallback_penalty, -self.fallback_penalty) } else { (REJECT, 0) };
        LanguageRule { trash_id: format_id(app, self.prefer), score, min_shift, profile_language: (app == App::Radarr).then_some("Any") }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LanguageRule {
    pub trash_id: &'static str,
    pub score: i32,
    pub min_shift: i32,
    pub profile_language: Option<&'static str>,
}

/// `docs/json/{radarr,sonarr}/cf/language-not-{english,original,french}.json`
/// at [`super::GUIDE_COMMIT`].
pub fn format_id(app: App, prefer: Preferred) -> &'static str {
    match (app, prefer) {
        (App::Radarr, Preferred::English) => "0dc8aec3bd1c47cd6c40c46ecd27e846",
        (App::Radarr, Preferred::Original) => "d6e9318c875905d6cfb5bee961afcea9",
        (App::Radarr, Preferred::French) => "533f782474f0819643c2ec0c1eeeb0ac",
        (App::Sonarr, Preferred::English) => "69aa1e159f97d860440b04cd6d590c4f",
        (App::Sonarr, Preferred::Original) => "ae575f95ab639ba5d15f663bf019e3e8",
        (App::Sonarr, Preferred::French) => "322fca6f8f3694acc1401ddb530fd33d",
    }
}
