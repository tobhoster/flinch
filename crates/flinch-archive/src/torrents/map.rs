//! Which torrents hold which cards, whether they hold the library's bytes,
//! and what that means for the plan.
//!
//! A card's torrents are the `downloadId`s of its newest imports (from *arr
//! history); a card with none found falls back to torrents whose content sits
//! inside its library folder. A torrent's file is "hardlinked to the library"
//! when it shares its device and inode with a file under the card's folder,
//! read through [`super::TorrentsConfig::local`]. A link that cannot be read
//! is unverified, never assumed absent.

use super::{ClientKind, Holding, SeedHold, Torrent, TorrentError, TorrentsConfig};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::os::unix::fs::MetadataExt;
use std::path::Path;

/// How deep under a library folder files are indexed (show/season/file).
const DEPTH: usize = 4;

/// How a torrent was tied to a card.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchedBy {
    History,
    Path,
}

/// Cards and the torrents found for them, before any file is read.
#[derive(Debug, Default)]
pub struct Matches<'a> {
    /// Card id → (client index, torrent, how).
    pub cards: BTreeMap<String, Vec<(usize, &'a Torrent, MatchedBy)>>,
    /// Cards whose history names a torrent that no readable client lists
    /// while some client could not be read: it may still be seeding.
    pub unreadable: HashSet<String>,
}

/// A torrent hash as the *arrs record it for a torrent client: 40 hex digits
/// (v1) or 64 (v2). Usenet ids look nothing like it.
pub fn is_torrent_hash(id: &str) -> bool {
    matches!(id.len(), 40 | 64) && id.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Card id → the download ids of the files on disk: per movie, and per
/// episode of a season, the newest import's. An older import was replaced by
/// an upgrade and its torrent no longer holds the library file. `instance` is
/// the one the records were read from ([`crate::ids`]).
pub fn downloads(records: &[crate::arr::history::HistoryRecord], instance: &str) -> BTreeMap<String, Vec<String>> {
    let mut newest: HashMap<(String, Option<u32>), (u64, String)> = HashMap::new();
    for (card, episode, at, id) in records.iter().filter_map(|record| record.import_download(instance)) {
        let slot = newest.entry((card, episode)).or_insert((at, id.clone()));
        if at > slot.0 {
            *slot = (at, id);
        }
    }
    let mut cards: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for ((card, _), (_, id)) in newest {
        cards.entry(card).or_default().push(id);
    }
    for ids in cards.values_mut() {
        ids.sort();
        ids.dedup();
    }
    cards
}

/// Tie cards to torrents. `downloads` is card id → lower-case download ids
/// ([`downloads`]); `folders` card id → library folder (*arr path);
/// `listings` is [`super::Torrents::list`]'s answer.
pub fn match_cards<'a>(
    downloads: &BTreeMap<String, Vec<String>>,
    folders: &HashMap<String, String>,
    listings: &'a [Result<Vec<Torrent>, TorrentError>],
    config: &TorrentsConfig,
) -> Matches<'a> {
    let mut by_hash: HashMap<&str, (usize, &Torrent)> = HashMap::new();
    for (index, listing) in listings.iter().enumerate() {
        for torrent in listing.iter().flatten() {
            by_hash.entry(torrent.hash.as_str()).or_insert((index, torrent));
        }
    }
    let any_failed = listings.iter().any(Result::is_err);
    let mut matches = Matches::default();
    for (card, ids) in downloads {
        for id in ids.iter().filter(|id| is_torrent_hash(id)) {
            match by_hash.get(id.as_str()) {
                Some(&(index, torrent)) => matches.cards.entry(card.clone()).or_default().push((index, torrent, MatchedBy::History)),
                None if any_failed => {
                    matches.unreadable.insert(card.clone());
                }
                None => {}
            }
        }
    }
    // The fallback: content inside the card's folder, for cards history tied
    // to no listed torrent. Each torrent is filed under every folder above it.
    let mut under: HashMap<std::path::PathBuf, Vec<(usize, &Torrent)>> = HashMap::new();
    for &(index, torrent) in by_hash.values() {
        for folder in config.local(&torrent.content_path).ancestors() {
            under.entry(folder.to_path_buf()).or_default().push((index, torrent));
        }
    }
    for (card, folder) in folders {
        if matches.cards.contains_key(card) {
            continue;
        }
        if let Some(inside) = under.get(&config.local(folder)) {
            matches.cards.insert(card.clone(), inside.iter().map(|&(index, torrent)| (index, torrent, MatchedBy::Path)).collect());
        }
    }
    for torrents in matches.cards.values_mut() {
        torrents.sort_by(|a, b| a.1.hash.cmp(&b.1.hash));
        torrents.dedup_by(|a, b| a.1.hash == b.1.hash);
    }
    matches
}

/// (device, inode) of every multiply-linked file under each folder, read
/// once per folder; `None` when the folder cannot be read.
#[derive(Default)]
pub struct LibraryLinks {
    folders: HashMap<std::path::PathBuf, Option<HashSet<(u64, u64)>>>,
}

impl LibraryLinks {
    fn of(&mut self, folder: &Path) -> Option<&HashSet<(u64, u64)>> {
        self.folders
            .entry(folder.to_path_buf())
            .or_insert_with(|| {
                let mut found = HashSet::new();
                index(folder, DEPTH, &mut found).ok().map(|()| found)
            })
            .as_ref()
    }
}

fn index(dir: &Path, depth: usize, found: &mut HashSet<(u64, u64)>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let meta = entry.metadata()?;
        if meta.is_dir() && depth > 0 {
            // A subfolder that cannot be read leaves its links unknown.
            index(&entry.path(), depth - 1, found)?;
        } else if meta.is_file() && meta.nlink() > 1 {
            found.insert((meta.dev(), meta.ino()));
        }
    }
    Ok(())
}

/// The torrent files (client paths) hardlinked into `folder`, and whether
/// every file could be checked. `files` is `None` when the client could not
/// list them.
pub fn hardlinks(files: Option<&[String]>, folder: &str, config: &TorrentsConfig, links: &mut LibraryLinks) -> (Vec<String>, bool) {
    let Some(files) = files else {
        return (Vec::new(), false);
    };
    let mut linked = Vec::new();
    let mut verified = !files.is_empty();
    for file in files {
        let Ok(meta) = std::fs::metadata(config.local(file)) else {
            verified = false;
            continue;
        };
        if meta.nlink() <= 1 {
            continue;
        }
        match links.of(&config.local(folder)) {
            Some(inodes) if inodes.contains(&(meta.dev(), meta.ino())) => linked.push(file.clone()),
            Some(_) => {}
            None => verified = false,
        }
    }
    (linked, verified)
}

/// A holding from a matched torrent; `cards` are all the cards it holds.
pub fn holding(
    index: usize,
    kind: ClientKind,
    torrent: &Torrent,
    cards: Vec<String>,
    links: (Vec<String>, bool),
    config: &TorrentsConfig,
) -> Holding {
    Holding {
        hash: torrent.hash.clone(),
        client: kind,
        client_index: index,
        ratio: torrent.ratio,
        seeding_secs: torrent.seeding_secs,
        meets_goal: config.meets_goal(torrent),
        hardlinked_paths: links.0,
        links_verified: links.1,
        cards,
    }
}

/// Why each card stays for its torrents. `torrent_goes` says the executor
/// removes a card's torrents with it: then a hardlinked torrent that may be
/// removed and holds no other card frees the bytes and holds nothing.
pub fn holds(
    holdings: &HashMap<String, Vec<Holding>>,
    unreadable: &HashSet<String>,
    config: &TorrentsConfig,
    torrent_goes: bool,
) -> HashMap<String, SeedHold> {
    let mut holds: HashMap<String, SeedHold> = unreadable.iter().map(|card| (card.clone(), SeedHold::ClientUnreadable)).collect();
    for (card, list) in holdings {
        if holds.contains_key(card) {
            continue;
        }
        let below = list.iter().filter(|holding| !holding.meets_goal).min_by(|a, b| a.ratio.total_cmp(&b.ratio));
        let hold = match below {
            Some(holding) if config.respect_seed_goals => Some(SeedHold::BelowGoal {
                ratio_centi: (holding.ratio.clamp(0.0, 9_999.0) * 100.0) as u32,
                seeding_days: (holding.seeding_secs / 86_400) as u32,
            }),
            _ => {
                let goes = |holding: &Holding| torrent_goes && config.may_remove(holding) && holding.cards.len() == 1;
                let staying: Vec<&Holding> = list.iter().filter(|holding| !goes(holding)).collect();
                if staying.iter().any(|holding| !holding.hardlinked_paths.is_empty()) {
                    Some(SeedHold::HeldByTorrent)
                } else if staying.iter().any(|holding| !holding.links_verified) {
                    Some(SeedHold::LinksUnverified)
                } else {
                    None
                }
            }
        };
        if let Some(hold) = hold {
            holds.insert(card.clone(), hold);
        }
    }
    holds
}

/// Cards a torrent holds below the desired ratio
/// ([`TorrentsConfig::prefer_after_ratio`]) and that no hold keeps already:
/// the planner spares them. Empty while the desired ratio is off.
pub fn spared(holdings: &HashMap<String, Vec<Holding>>, holds: &HashMap<String, SeedHold>, config: &TorrentsConfig) -> HashSet<String> {
    if config.prefer_after_ratio <= 0.0 {
        return HashSet::new();
    }
    holdings
        .iter()
        .filter(|(card, list)| !holds.contains_key(*card) && list.iter().any(|holding| holding.ratio < config.prefer_after_ratio))
        .map(|(card, _)| card.clone())
        .collect()
}

/// Items and bytes behind one kind of hold.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Held {
    pub items: usize,
    pub bytes: u64,
}

/// One client as this cycle read it. `host` is the URL's host and port only.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClientStatus {
    pub kind: ClientKind,
    pub host: String,
    pub torrents: usize,
    #[serde(default)]
    pub error: Option<String>,
}

/// `status.json` `torrents`: what the clients hold and what that keeps.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TorrentStatus {
    pub clients: Vec<ClientStatus>,
    /// Items with at least one torrent, by how the first was found.
    pub by_history: usize,
    pub by_path: usize,
    pub below_goal: Held,
    pub held_by_torrent: Held,
    pub links_unverified: Held,
    pub client_unreadable: Held,
    /// Below the desired ratio: not kept, drawn on last.
    pub spared: Held,
    /// Whether the executor removes an item's torrents with it.
    pub torrents_go_with_items: bool,
}

impl TorrentStatus {
    /// Count each hold against the card's size.
    pub fn count(&mut self, holds: &HashMap<String, SeedHold>, size: impl Fn(&str) -> u64) {
        for (card, hold) in holds {
            let held = match hold {
                SeedHold::BelowGoal { .. } => &mut self.below_goal,
                SeedHold::HeldByTorrent => &mut self.held_by_torrent,
                SeedHold::LinksUnverified => &mut self.links_unverified,
                SeedHold::ClientUnreadable => &mut self.client_unreadable,
            };
            held.items += 1;
            held.bytes = held.bytes.saturating_add(size(card));
        }
    }
}
