//! Themes: the library grouped by what its titles are about, so the operator
//! sees where the disk goes ("Crime · Drama holds 2 TiB, 4% of it played in a
//! year") and quality advice can tell a corner nobody visits from one the
//! household lives in.
//!
//! A theme is a cluster of taste vectors ([`crate::embedding`]): spherical
//! k-means (cosine similarity, centroids re-normalised every round) with
//! k ≈ √(n/2), clamped to [`MIN_THEMES`]..=[`MAX_THEMES`]. The same vectors
//! always give the same themes: subjects are taken in id order, k-means++
//! seeding draws from a fixed-seed generator, ties go to the lower index and
//! the rounds are capped. Each theme is named from the *arr genres its members
//! carry most often; no language model is involved.
//!
//! Themes are advice only. They feed the published storage view and a
//! downgrade suggestion in [`crate::quality`]; they never change regret or
//! the plan. A title without a vector belongs to no theme and is never in a
//! cold one, and without a single play in the window no theme is cold:
//! missing evidence is not read as disinterest.

use crate::embedding::subject_of;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// The assignments, in the state directory, kept between cycles.
pub const THEMES_FILE: &str = "themes.json";
/// Bounds on the number of themes.
pub const MIN_THEMES: usize = 4;
pub const MAX_THEMES: usize = 24;
/// Fewer subjects with a vector than this are not clustered: four themes of
/// a title or two each describe nothing.
pub const MIN_SUBJECTS: usize = 4 * MIN_THEMES;
/// Rounds of k-means at most; it stops earlier once no subject moves.
const ROUNDS: usize = 30;
const SEED: u64 = 0x7E3E_5EED_F11C_0001;
/// Themes are recomputed at least this often, and whenever a vector changes.
pub const RECLUSTER_AFTER_S: u64 = 86_400;
/// A title counts as played when anyone played it this recently.
pub const PLAYED_WINDOW_S: u64 = 365 * 86_400;
/// A theme whose played share is below this is cold.
pub const COLD_PLAYED_SHARE: f64 = 0.10;
/// Fewer on-disk titles than this never make a theme cold: two unplayed
/// films are not a pattern.
pub const COLD_MIN_TITLES: usize = 5;
/// A genre carried by at least this share of a theme's members joins its
/// name; the most common one always does.
const NAME_SHARE: f64 = 1.0 / 3.0;
const NAME_GENRES: usize = 2;
const UNNAMED: &str = "Unlabelled";

/// One movie or show to cluster: its unit vector and its *arr genres.
#[derive(Debug, Clone, Copy)]
pub struct Member<'a> {
    pub subject: &'a str,
    pub vector: &'a [f32],
    pub genres: &'a [String],
}

/// Theme names and every clustered subject's theme, as `themes.json` keeps
/// them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Themes {
    /// When they were computed, unix seconds.
    #[serde(default)]
    pub computed_at: u64,
    /// [`fingerprint`] of the members clustered.
    #[serde(default)]
    pub fingerprint: u64,
    /// Theme index → name, largest theme first.
    #[serde(default)]
    pub names: Vec<String>,
    /// Subject → theme index.
    #[serde(default)]
    pub assignments: BTreeMap<String, usize>,
}

impl Themes {
    /// Cluster `members` and name the themes. Fewer than [`MIN_SUBJECTS`]
    /// usable members give no themes.
    pub fn compute(members: &[Member<'_>], now: u64) -> Self {
        let members = ordered(members);
        let fingerprint = hash(&members);
        if members.len() < MIN_SUBJECTS {
            return Self { computed_at: now, fingerprint, ..Self::default() };
        }
        let points: Vec<&[f32]> = members.iter().map(|member| member.vector).collect();
        let k = theme_count(points.len());
        let mut groups: Vec<Vec<usize>> = vec![Vec::new(); k];
        for (index, label) in cluster(&points, k).into_iter().enumerate() {
            groups[label].push(index);
        }
        groups.retain(|group| !group.is_empty());
        // Largest first; members are in id order, so the first member breaks ties.
        groups.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a[0].cmp(&b[0])));
        let mut names = Vec::with_capacity(groups.len());
        let mut assignments = BTreeMap::new();
        for (theme, group) in groups.iter().enumerate() {
            let genres: Vec<&[String]> = group.iter().map(|&index| members[index].genres).collect();
            names.push(name(&genres, &names));
            assignments.extend(group.iter().map(|&index| (members[index].subject.to_string(), theme)));
        }
        Self { computed_at: now, fingerprint, names, assignments }
    }

    /// Whether these themes were made from members fingerprinting to
    /// `fingerprint` less than [`RECLUSTER_AFTER_S`] before `now`.
    pub fn is_current(&self, fingerprint: u64, now: u64) -> bool {
        self.fingerprint == fingerprint && now.saturating_sub(self.computed_at) < RECLUSTER_AFTER_S
    }

    /// The name of `subject`'s theme (see [`subject_of`]).
    pub fn name_of(&self, subject: &str) -> Option<&str> {
        self.assignments.get(subject).and_then(|&theme| self.names.get(theme)).map(String::as_str)
    }
}

/// Members in subject order, one per subject, without empty vectors.
fn ordered<'m, 'a>(members: &'m [Member<'a>]) -> Vec<&'m Member<'a>> {
    let mut ordered: Vec<&Member> = members.iter().filter(|member| !member.vector.is_empty()).collect();
    ordered.sort_by(|a, b| a.subject.cmp(b.subject));
    ordered.dedup_by(|a, b| a.subject == b.subject);
    ordered
}

/// What the themes depend on: every member's subject and vector, in any
/// order. Equal fingerprints cluster alike; genres only name.
pub fn fingerprint(members: &[Member<'_>]) -> u64 {
    hash(&ordered(members))
}

/// FNV-1a, stable across builds and platforms (unlike the std hasher).
fn hash(members: &[&Member<'_>]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    let mut eat = |bytes: &[u8]| {
        for byte in bytes {
            hash = (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    for member in members {
        eat(member.subject.as_bytes());
        eat(&[0xff]);
        for value in member.vector {
            eat(&value.to_bits().to_le_bytes());
        }
    }
    hash
}

/// k ≈ √(n/2), within the bounds and never above `n`.
fn theme_count(subjects: usize) -> usize {
    ((subjects as f64 / 2.0).sqrt().round() as usize).clamp(MIN_THEMES, MAX_THEMES).min(subjects)
}

/// Spherical k-means: each point's cluster index, every one below `k`.
fn cluster(points: &[&[f32]], k: usize) -> Vec<usize> {
    let mut centroids = seeds(points, k);
    let mut labels = vec![usize::MAX; points.len()];
    for _ in 0..ROUNDS {
        let next: Vec<usize> = points.iter().map(|point| nearest(point, &centroids)).collect();
        if next == labels {
            break;
        }
        labels = next;
        recentre(points, &labels, &mut centroids);
    }
    labels
}

fn dot(a: &[f32], b: &[f32]) -> f64 {
    a.iter().zip(b).map(|(x, y)| f64::from(*x) * f64::from(*y)).sum()
}

/// The most similar centroid; the lower index on a tie.
fn nearest(point: &[f32], centroids: &[Vec<f32>]) -> usize {
    let mut best = (0, f64::NEG_INFINITY);
    for (index, centroid) in centroids.iter().enumerate() {
        let similarity = dot(point, centroid);
        if similarity > best.1 {
            best = (index, similarity);
        }
    }
    best.0
}

/// k-means++ seeding under cosine distance, drawn from a fixed-seed
/// generator. Stops short of `k` when every point sits on a seed already.
fn seeds(points: &[&[f32]], k: usize) -> Vec<Vec<f32>> {
    let mut random = SplitMix(SEED);
    let first = ((random.unit() * points.len() as f64) as usize).min(points.len().saturating_sub(1));
    let Some(first) = points.get(first) else { return Vec::new() };
    let mut centroids = vec![first.to_vec()];
    let mut distance: Vec<f64> = points.iter().map(|point| (1.0 - dot(point, first)).max(0.0)).collect();
    while centroids.len() < k {
        let weights: Vec<f64> = distance.iter().map(|d| d * d).collect();
        let total: f64 = weights.iter().sum();
        let Some(last) = weights.iter().rposition(|w| *w > 0.0) else { break };
        let mut target = random.unit() * total;
        let mut pick = last;
        for (index, weight) in weights.iter().enumerate() {
            if *weight > 0.0 && target < *weight {
                pick = index;
                break;
            }
            target -= weight;
        }
        let seed = points[pick];
        for (d, point) in distance.iter_mut().zip(points) {
            *d = d.min((1.0 - dot(point, seed)).max(0.0));
        }
        centroids.push(seed.to_vec());
    }
    centroids
}

/// Each centroid becomes its members' mean direction; a centroid left with
/// no member keeps its place.
fn recentre(points: &[&[f32]], labels: &[usize], centroids: &mut [Vec<f32>]) {
    let dimensions = centroids.first().map_or(0, Vec::len);
    let mut sums = vec![vec![0.0f64; dimensions]; centroids.len()];
    for (point, &label) in points.iter().zip(labels) {
        for (sum, value) in sums[label].iter_mut().zip(*point) {
            *sum += f64::from(*value);
        }
    }
    for (centroid, sum) in centroids.iter_mut().zip(sums) {
        let norm = sum.iter().map(|s| s * s).sum::<f64>().sqrt();
        if norm > 0.0 {
            *centroid = sum.iter().map(|s| (s / norm) as f32).collect();
        }
    }
}

/// SplitMix64: a tiny, fixed generator, so seeding needs no dependency and
/// never varies between builds.
struct SplitMix(u64);

impl SplitMix {
    /// Uniform in [0, 1).
    fn unit(&mut self) -> f64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^= z >> 31;
        (z >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// A theme's name from its members' genres (one slice per member): the most
/// common genre, and the next one when at least a third of the members carry
/// it, joined by " · "; ties go alphabetically. A name in `taken` grows by the
/// next genre, then by a number. No genres at all reads "Unlabelled".
pub fn name(members: &[&[String]], taken: &[String]) -> String {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for genres in members {
        let distinct: BTreeSet<&str> = genres.iter().map(|genre| genre.trim()).filter(|genre| !genre.is_empty()).collect();
        for genre in distinct {
            *counts.entry(genre).or_default() += 1;
        }
    }
    // Stable sort of an alphabetical list: equal counts stay alphabetical.
    let mut ranked: Vec<(&str, usize)> = counts.into_iter().collect();
    ranked.sort_by_key(|entry| std::cmp::Reverse(entry.1));
    let genres: Vec<&str> = ranked.iter().map(|(genre, _)| *genre).collect();
    let common = ranked
        .iter()
        .take(NAME_GENRES)
        .enumerate()
        .take_while(|(rank, (_, count))| *rank == 0 || *count as f64 >= NAME_SHARE * members.len() as f64)
        .count();
    let base = if genres.is_empty() { UNNAMED.to_string() } else { genres[..common].join(" · ") };
    let free = |candidate: &String| !taken.contains(candidate);
    if free(&base) {
        return base;
    }
    if let Some(longer) = genres.get(..common + 1).map(|names| names.join(" · ")).filter(free) {
        return longer;
    }
    (2..).map(|number| format!("{base} {number}")).find(free).unwrap_or_else(|| UNNAMED.to_string())
}

/// One on-disk movie or season, as the storage view reads it.
#[derive(Debug, Clone, Copy)]
pub struct Holding<'a> {
    pub card_id: &'a str,
    pub bytes: u64,
    /// The newest play of it or, for a season, of any season of its show.
    pub last_play: Option<u64>,
    /// This cycle's plan evicts it.
    pub planned: bool,
}

/// One theme in the storage view.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ThemeStats {
    pub name: String,
    /// Movies and shows with bytes on disk.
    pub titles: usize,
    pub bytes: u64,
    /// Of those, the ones anyone played within [`PLAYED_WINDOW_S`].
    pub played: usize,
    /// `played / titles`.
    pub played_share: f64,
    /// Bytes this cycle's plan evicts.
    pub planned_bytes: u64,
    /// Seldom played: its large items are advised a downgrade.
    pub cold: bool,
}

/// Storage by theme, as status.json publishes it (`themes`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ThemesStatus {
    /// When the themes were computed, unix seconds.
    pub computed_at: u64,
    /// Largest on disk first.
    pub themes: Vec<ThemeStats>,
    /// On-disk titles without a vector, hence without a theme, and their bytes.
    #[serde(default)]
    pub unthemed_titles: usize,
    #[serde(default)]
    pub unthemed_bytes: u64,
}

/// A theme the household seldom plays from, as quality advice reads it.
#[derive(Debug, Clone, PartialEq)]
pub struct ColdTheme {
    pub name: String,
    pub played_share: f64,
}

/// Bytes, titles, played share and planned evictions per theme, as of `now`.
pub fn status(themes: &Themes, holdings: &[Holding<'_>], now: u64) -> ThemesStatus {
    #[derive(Default)]
    struct Tally<'a> {
        titles: BTreeSet<&'a str>,
        played: BTreeSet<&'a str>,
        bytes: u64,
        planned_bytes: u64,
    }
    let since = now.saturating_sub(PLAYED_WINDOW_S);
    let mut tallies: Vec<Tally> = themes.names.iter().map(|_| Tally::default()).collect();
    let mut unthemed: (BTreeSet<&str>, u64) = (BTreeSet::new(), 0);
    let mut anything_played = false;
    for holding in holdings.iter().filter(|holding| holding.bytes > 0) {
        let subject = subject_of(holding.card_id);
        let played = holding.last_play.is_some_and(|epoch| epoch >= since);
        anything_played |= played;
        let Some(tally) = themes.assignments.get(subject).and_then(|&theme| tallies.get_mut(theme)) else {
            unthemed.0.insert(subject);
            unthemed.1 += holding.bytes;
            continue;
        };
        tally.titles.insert(subject);
        if played {
            tally.played.insert(subject);
        }
        tally.bytes += holding.bytes;
        if holding.planned {
            tally.planned_bytes += holding.bytes;
        }
    }
    let mut stats: Vec<ThemeStats> = themes
        .names
        .iter()
        .zip(tallies)
        .filter(|(_, tally)| !tally.titles.is_empty())
        .map(|(name, tally)| {
            let played_share = tally.played.len() as f64 / tally.titles.len() as f64;
            ThemeStats {
                name: name.clone(),
                titles: tally.titles.len(),
                bytes: tally.bytes,
                played: tally.played.len(),
                played_share,
                planned_bytes: tally.planned_bytes,
                cold: anything_played && tally.titles.len() >= COLD_MIN_TITLES && played_share < COLD_PLAYED_SHARE,
            }
        })
        .collect();
    stats.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.name.cmp(&b.name)));
    ThemesStatus { computed_at: themes.computed_at, themes: stats, unthemed_titles: unthemed.0.len(), unthemed_bytes: unthemed.1 }
}

/// Card id → its theme, for every card of `card_ids` in a cold theme.
pub fn cold_cards<'a>(themes: &Themes, status: &ThemesStatus, card_ids: impl IntoIterator<Item = &'a str>) -> HashMap<String, ColdTheme> {
    let cold: HashMap<&str, f64> =
        status.themes.iter().filter(|theme| theme.cold).map(|theme| (theme.name.as_str(), theme.played_share)).collect();
    card_ids
        .into_iter()
        .filter_map(|card_id| {
            let name = themes.name_of(subject_of(card_id))?;
            let played_share = *cold.get(name)?;
            Some((card_id.to_string(), ColdTheme { name: name.to_string(), played_share }))
        })
        .collect()
}

#[cfg(test)]
mod tests;
