//! The greedy pass and the release order: a priority queue over units whose
//! prerequisites are taken, ranked by rule tier, hand-over and bytes per
//! regret. The emergency (and HiGHS-failed) selection, and the order every
//! plan leaves in.

use super::{objective_weight, quantize, Force, Unit};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BinaryHeap, HashMap};

/// Bytes per unit of regret, with ε so a zero-regret unit ranks first instead
/// of dividing by zero.
fn efficiency(unit: &Unit) -> f64 {
    const EPSILON: f64 = 1e-9;
    unit.size_bytes as f64 / (objective_weight(unit) + EPSILON)
}

struct Ready {
    index: usize,
    tier: u8,
    handed: bool,
    efficiency: f64,
}

impl PartialEq for Ready {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}
impl Eq for Ready {}
impl PartialOrd for Ready {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Ready {
    /// Max-heap order: the rule tier first (see [`tiers`]), then handed, then
    /// most bytes per regret, then the lower index, so the pass is
    /// reproducible.
    fn cmp(&self, other: &Self) -> Ordering {
        self.tier
            .cmp(&other.tier)
            .then(self.handed.cmp(&other.handed))
            .then(self.efficiency.total_cmp(&other.efficiency))
            .then(other.index.cmp(&self.index))
    }
}

/// The tier of a must-go unit and of every prerequisite its chain gives it.
const MUST: u8 = 3;

/// Each unit's rank under the operator's rules: [`MUST`] for a must-go open
/// unit and the open units it depends on, 2 for a preferred one, 0 for a
/// spared one, else 1.
pub(super) fn tiers(units: &[Unit], open: &[bool], edges: &[(usize, usize)]) -> Vec<u8> {
    let blocked_by: HashMap<usize, usize> = edges.iter().copied().collect();
    let mut tier: Vec<u8> = units
        .iter()
        .zip(open)
        .map(|(unit, &open)| match (open, unit.force) {
            (true, Some(Force::Must)) => MUST,
            (true, Some(Force::Prefer)) => 2,
            (true, Some(Force::Spare)) => 0,
            _ => 1,
        })
        .collect();
    for index in 0..units.len() {
        if tier[index] != MUST {
            continue;
        }
        // A prerequisite already at MUST has its own chain walked.
        let mut next = blocked_by.get(&index).copied();
        while let Some(prerequisite) = next.filter(|&prerequisite| tier[prerequisite] != MUST) {
            tier[prerequisite] = MUST;
            next = blocked_by.get(&prerequisite).copied();
        }
    }
    tier
}

/// What [`release`] does with the best ready unit.
pub(super) enum Step {
    Take,
    Skip,
    Stop,
}

/// Walk `open` units in precedence order, best ready first: a unit is ready
/// once its prerequisite was taken. Returns each taken unit with its
/// prerequisite. O(N log N).
pub(super) fn release(
    units: &[Unit],
    open: &[bool],
    edges: &[(usize, usize)],
    tier: &[u8],
    mut step: impl FnMut(usize) -> Step,
) -> Vec<(usize, Option<usize>)> {
    // Each unit has at most one prerequisite (chains), and unlocks at most one.
    let mut blocked_by: HashMap<usize, usize> = HashMap::new();
    let mut unlocks: HashMap<usize, usize> = HashMap::new();
    for &(dependent, prerequisite) in edges {
        blocked_by.insert(dependent, prerequisite);
        unlocks.insert(prerequisite, dependent);
    }
    let ready = |index: usize| Ready { index, tier: tier[index], handed: units[index].handed, efficiency: efficiency(&units[index]) };
    let mut heap: BinaryHeap<Ready> = (0..units.len()).filter(|index| open[*index] && !blocked_by.contains_key(index)).map(ready).collect();
    let mut taken = Vec::new();
    while let Some(Ready { index, .. }) = heap.pop() {
        match step(index) {
            Step::Stop => break,
            Step::Skip => continue,
            Step::Take => {}
        }
        taken.push((index, blocked_by.get(&index).copied()));
        if let Some(&next) = unlocks.get(&index) {
            heap.push(ready(next));
        }
    }
    taken
}

/// Greedy cover: every must-go unit and its prerequisites, then the best ready
/// unit on a volume still short of its target.
pub(super) fn greedy(units: &[Unit], open: &[bool], edges: &[(usize, usize)], need: &BTreeMap<&str, u64>, quantum: u64) -> Vec<usize> {
    let tier = tiers(units, open, edges);
    let mut must_left = tier.iter().filter(|&&tier| tier == MUST).count();
    let mut covered: BTreeMap<&str, u64> = BTreeMap::new();
    let short =
        |covered: &BTreeMap<&str, u64>, volume: &str| covered.get(volume).copied().unwrap_or(0) < need.get(volume).copied().unwrap_or(0);
    let mut chosen: Vec<usize> = release(units, open, edges, &tier, |index| {
        let volume = units[index].volume.as_str();
        if tier[index] == MUST {
            must_left = must_left.saturating_sub(1);
        } else if must_left == 0 && need.keys().all(|volume| !short(&covered, volume)) {
            return Step::Stop;
        } else if !short(&covered, volume) {
            return Step::Skip;
        }
        *covered.entry(volume).or_insert(0) += quantize(units[index].size_bytes, quantum);
        Step::Take
    })
    .into_iter()
    .map(|(index, _)| index)
    .collect();
    chosen.sort_unstable();
    chosen
}
