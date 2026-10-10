//! Group copies by catalogue id. Pure: the daemon reads Plex and the *arrs,
//! this decides what is a duplicate.
//!
//! A copy is a physical file set. Two Plex `Media` that share a file (one
//! folder in two libraries, or one file seen through two mounts) are one
//! copy: removing either would remove both, so they never form a group. A
//! Plex copy is an *arr's file when a path matches, or — the *arrs and Plex
//! often mount the store under different roots — when file name and size
//! both match. An *arr file that matches no copy Plex lists is a copy of its
//! own only when Plex lists none; otherwise it may be one of Plex's copies
//! under another name, so the group is held: removing through Plex could hit
//! the *arr's file.

use super::pick::{self, Advice};
use super::{ArrFile, Copy, Group, KeepPreference, Source};
use crate::plex::PlexMetadata;
use std::collections::{BTreeMap, HashMap};

/// One Plex `Media` of a movie item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlexVersion {
    pub rating_key: String,
    pub section_id: Option<u32>,
    pub media_id: u64,
    pub resolution: Option<String>,
    /// Each part's file and size.
    pub files: Vec<(String, u64)>,
    /// Plays logged under the ratingKey.
    pub plays: u32,
}

impl PlexVersion {
    /// Every `Media` of a movie row; one without an id cannot be removed or
    /// told apart, and is left out.
    pub fn of(row: &PlexMetadata, plays: u32) -> Vec<Self> {
        row.media
            .iter()
            .filter_map(|media| {
                Some(Self {
                    rating_key: row.rating_key.clone(),
                    section_id: row.library_section_id,
                    media_id: media.id?,
                    resolution: media.video_resolution.as_deref().and_then(resolution),
                    files: media
                        .parts
                        .iter()
                        .filter_map(|part| Some((part.file.clone().filter(|file| !file.is_empty())?, part.size.unwrap_or(0))))
                        .collect(),
                    plays,
                })
            })
            .collect()
    }
}

/// One movie as one source sees it; movies sharing a tmdb id merge.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MovieIn {
    pub card_id: Option<String>,
    pub title: String,
    pub year: Option<u32>,
    pub tmdb: Option<u32>,
    pub plex: Vec<PlexVersion>,
    pub arr: Vec<ArrFile>,
}

impl MovieIn {
    fn key(&self) -> Option<String> {
        match self.tmdb {
            Some(tmdb) => Some(format!("tmdb:{tmdb}")),
            None => self.plex.first().map(|version| format!("plex:{}", version.rating_key)),
        }
    }
}

/// Plex's `videoResolution` (`4k`, `1080`, `720`, `576`, `sd`) or an *arr
/// quality name (`Bluray-2160p`) as `2160`, `1080`, `720` or `sd`.
pub fn resolution(text: &str) -> Option<String> {
    let text = text.trim().to_ascii_lowercase();
    if text.contains("4k") || text.contains("2160") {
        return Some("2160".into());
    }
    if text.contains("1080") {
        return Some("1080".into());
    }
    if text.contains("720") {
        return Some("720".into());
    }
    ["sd", "480", "576", "dvd"].iter().any(|sd| text.contains(sd)).then(|| "sd".into())
}

fn base_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// Whether a Plex file is the *arr's: same path, or same name and size.
fn same_file(plex: &(String, u64), arr: &ArrFile) -> bool {
    plex.0 == arr.path || (plex.1 > 0 && plex.1 == arr.bytes && base_name(&plex.0) == base_name(&arr.path))
}

/// Duplicate groups among `movies`, sorted by redundant bytes, largest first.
/// `advice` is the quality advice by card id.
pub fn group(movies: Vec<MovieIn>, prefer: KeepPreference, advice: &HashMap<String, Advice>) -> Vec<Group> {
    let mut merged: BTreeMap<String, MovieIn> = BTreeMap::new();
    for movie in movies {
        let Some(key) = movie.key() else { continue };
        match merged.get_mut(&key) {
            Some(into) => {
                into.card_id = into.card_id.take().or(movie.card_id);
                into.year = into.year.or(movie.year);
                if into.title.is_empty() {
                    into.title = movie.title;
                }
                into.plex.extend(movie.plex);
                into.arr.extend(movie.arr);
            }
            None => {
                merged.insert(key, movie);
            }
        }
    }
    let mut groups: Vec<Group> = merged.into_iter().filter_map(|(id, movie)| one(id, movie, prefer, advice)).collect();
    groups.sort_by(|a, b| b.redundant_bytes.cmp(&a.redundant_bytes).then_with(|| a.id.cmp(&b.id)));
    groups
}

/// Recommend again with the quality advice, which is known only once the
/// cycle has scored its candidates.
pub fn advise(groups: &mut [Group], prefer: KeepPreference, advice: &HashMap<String, Advice>) {
    for group in groups {
        let advice = group.card_id.as_ref().and_then(|card| advice.get(card)).copied().unwrap_or_default();
        let (recommended, reasons) = pick::recommend(&group.copies, prefer, advice);
        group.redundant_bytes = group.copies.iter().filter(|copy| copy.id != recommended).map(|copy| copy.bytes).sum();
        group.recommended = recommended;
        group.reasons = reasons;
    }
}

/// The group of one merged movie; `None` with fewer than two copies.
fn one(id: String, movie: MovieIn, prefer: KeepPreference, advice: &HashMap<String, Advice>) -> Option<Group> {
    let (copies, unmatched) = copies(&movie);
    if copies.len() < 2 {
        return None;
    }
    let advice = movie.card_id.as_ref().and_then(|card| advice.get(card)).copied().unwrap_or_default();
    let (recommended, reasons) = pick::recommend(&copies, prefer, advice);
    let redundant_bytes = copies.iter().filter(|copy| copy.id != recommended).map(|copy| copy.bytes).sum();
    let held = unmatched.first().map(|path| {
        format!(
            "{path} is an *arr's file but matches no Plex copy by path, or by name and size: FLINCH cannot tell which copy the *arr tracks"
        )
    });
    Some(Group {
        id,
        card_id: movie.card_id,
        title: movie.title,
        year: movie.year,
        copies,
        recommended,
        reasons,
        decision: None,
        held,
        redundant_bytes,
    })
}

/// The physical copies of one merged movie, and the paths of *arr files
/// that match none of the copies Plex lists.
fn copies(movie: &MovieIn) -> (Vec<Copy>, Vec<String>) {
    let mut copies: Vec<Copy> = Vec::new();
    let mut seen_files: Vec<&str> = Vec::new();
    let mut owned: Vec<(&str, u32)> = Vec::new();
    for version in &movie.plex {
        let id = format!("plex:{}:{}", version.rating_key, version.media_id);
        if copies.iter().any(|copy| copy.id == id) || version.files.iter().any(|(file, _)| seen_files.contains(&file.as_str())) {
            continue;
        }
        let owner = movie.arr.iter().find(|arr| version.files.iter().any(|file| same_file(file, arr)));
        if let Some(arr) = owner {
            if owned.contains(&(arr.instance.as_str(), arr.file_id)) {
                continue;
            }
            owned.push((arr.instance.as_str(), arr.file_id));
        }
        seen_files.extend(version.files.iter().map(|(file, _)| file.as_str()));
        copies.push(Copy {
            id,
            source: Source::Plex,
            rating_key: Some(version.rating_key.clone()),
            section_id: version.section_id,
            media_id: Some(version.media_id),
            owner: owner.cloned(),
            resolution: version.resolution.clone().or_else(|| owner.and_then(|arr| arr.quality.as_deref()).and_then(resolution)),
            bytes: version.files.iter().map(|(_, size)| size).sum::<u64>().max(owner.map_or(0, |arr| arr.bytes)),
            file: version.files.first().map(|(file, _)| file.clone()),
            plays: version.plays,
        });
    }
    let mut unmatched = Vec::new();
    for arr in &movie.arr {
        if owned.contains(&(arr.instance.as_str(), arr.file_id)) {
            continue;
        }
        if !movie.plex.is_empty() {
            unmatched.push(arr.path.clone());
            continue;
        }
        owned.push((arr.instance.as_str(), arr.file_id));
        copies.push(Copy {
            id: format!("{}:{}:{}", arr.instance, arr.movie_id, arr.file_id),
            source: Source::Arr,
            rating_key: None,
            section_id: None,
            media_id: None,
            owner: Some(arr.clone()),
            resolution: arr.quality.as_deref().and_then(resolution),
            bytes: arr.bytes,
            file: Some(arr.path.clone()),
            plays: 0,
        });
    }
    (copies, unmatched)
}
