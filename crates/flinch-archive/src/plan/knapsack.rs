//! The selection core: which candidates free each volume's byte target at the
//! least total regret, without orphaning part of a show.
//!
//! A 0-1 knapsack in its covering form, one capacity row per volume:
//!
//! ```text
//! min  Σ R_i x_i
//! s.t. Σ_{i on v} Ŝ_i x_i ≥ B̂_v      for every volume v with a target
//!      x_a ≤ x_b                      for every precedence edge a → b
//!      x_i ∈ {0, 1}
//! ```
//!
//! Sizes and targets are quantized to `quantum` bytes (`Ŝ = ⌈S/Q⌉`,
//! `B̂ = ⌈B/Q⌉`) so coefficients stay small integers. HiGHS solves it exactly;
//! in an emergency, or if HiGHS fails, a greedy pass over a priority queue
//! takes the most bytes per unit of regret first, honouring the same edges.
//!
//! Precedence, per show, over its seasons (or any ordered units):
//! - **Unplayed** units go from the end: an earlier one may go only once every
//!   later unplayed one has (`x_j ≤ x_{j+1}`). The start of a show nobody has
//!   begun is the last thing evicted.
//! - **Played** units go from the start: a later one may go only once every
//!   earlier played one has (`x_{j+1} ≤ x_j`). The oldest watched go first.
//!
//! A unit that cannot be selected (pinned, in its grace period, off any
//! target volume) is a fixed 0 in its chain, so everything its edges make
//! depend on it is unselectable too: excluding season 5 of an unplayed show
//! protects seasons 1-4 rather than evicting around it.

use good_lp::{constraint, highs, variable, Expression, ProblemVariables, Solution, SolverModel, Variable};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BinaryHeap, HashMap};

/// Where a unit sits in its show's order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sequence {
    /// The show (or any ordering group). Units order only within one group.
    pub group: String,
    /// Position in the group: the season number for seasons.
    pub index: u32,
    /// Anyone has played this unit. Played and unplayed units form separate
    /// chains with opposite directions.
    pub played: bool,
}

/// One unit the solver may select. Built by the caller after hard
/// exclusions; `selectable == false` keeps an excluded unit in its chain as a
/// fixed 0.
#[derive(Debug, Clone, PartialEq)]
pub struct Unit {
    pub size_bytes: u64,
    /// Expected regret of evicting it: finite and ≥ 0.
    pub regret: f64,
    /// Volume key whose target this unit's bytes count toward.
    pub volume: String,
    pub sequence: Option<Sequence>,
    pub selectable: bool,
    /// Already handed over in an earlier cycle: preferred at equal cost, so a
    /// re-plan does not restart its Leaving Soon window.
    pub handed: bool,
}

/// How the selection was made.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Method {
    /// HiGHS solved the 0-1 program to optimality.
    Milp,
    /// Greedy: a volume is at or over the emergency ratio.
    Emergency,
    /// Greedy: HiGHS failed; the selection is feasible but may not be optimal.
    SolverFallback,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Selection {
    /// Indices into the unit slice, ascending.
    pub chosen: Vec<usize>,
    pub method: Method,
    /// Why HiGHS was not used, when it failed.
    pub solver_error: Option<String>,
}

/// Each show's units as chains oriented "each member depends on the next":
/// unplayed ascending (`x_j ≤ x_{j+1}`), played descending (`x_{j+1} ≤ x_j`).
fn oriented_chains(units: &[Unit]) -> Vec<Vec<usize>> {
    let mut groups: BTreeMap<(&str, bool), Vec<usize>> = BTreeMap::new();
    for (index, unit) in units.iter().enumerate() {
        if let Some(sequence) = &unit.sequence {
            groups.entry((sequence.group.as_str(), sequence.played)).or_default().push(index);
        }
    }
    groups
        .into_iter()
        .map(|((_, played), mut members)| {
            members.sort_by_key(|&index| units[index].sequence.as_ref().map_or(0, |sequence| sequence.index));
            if played {
                members.reverse();
            }
            members
        })
        .collect()
}

/// The precedence edges `a → b` meaning `x_a ≤ x_b` (a may go only if b does)
/// among `open` units, after closing everything a closed unit's chain makes
/// depend on it.
fn edges(units: &[Unit], open: &mut [bool]) -> Vec<(usize, usize)> {
    let mut edges = Vec::new();
    for members in oriented_chains(units) {
        // A closed member closes everything before it in this orientation.
        if let Some(last_closed) = members.iter().rposition(|&index| !open[index]) {
            for &index in &members[..last_closed] {
                open[index] = false;
            }
        }
        edges.extend(members.windows(2).filter(|pair| open[pair[0]] && open[pair[1]]).map(|pair| (pair[0], pair[1])));
    }
    edges
}

/// Selectable units and their edges. A unit on a volume with no target is a
/// fixed zero: nothing asks for its bytes, so it stays, and so does everything
/// that depends on it.
fn chains(units: &[Unit], targets: &BTreeMap<String, u64>) -> (Vec<(usize, usize)>, Vec<bool>) {
    let wanted = |unit: &Unit| targets.get(&unit.volume).is_some_and(|bytes| *bytes > 0);
    let mut open: Vec<bool> = units.iter().map(|unit| unit.selectable && unit.size_bytes > 0 && wanted(unit)).collect();
    let edges = edges(units, &mut open);
    (edges, open)
}

/// Bytes rounded up to whole quanta.
pub fn quantize(bytes: u64, quantum: u64) -> u64 {
    bytes.div_ceil(quantum.max(1))
}

/// Select units covering every volume target at least total regret.
///
/// A target larger than everything selectable on its volume is capped to it:
/// the plan then takes everything it may there and reports the shortfall,
/// instead of failing the whole program as infeasible. `emergency` skips
/// HiGHS for the greedy pass.
pub fn select(units: &[Unit], targets: &BTreeMap<String, u64>, quantum: u64, emergency: bool) -> Selection {
    let (edges, open) = chains(units, targets);
    let need: BTreeMap<&str, u64> = targets
        .iter()
        .filter(|(_, bytes)| **bytes > 0)
        .map(|(volume, bytes)| {
            let reachable: u64 = units
                .iter()
                .zip(&open)
                .filter(|(unit, open)| **open && unit.volume == *volume)
                .map(|(unit, _)| quantize(unit.size_bytes, quantum))
                .sum();
            (volume.as_str(), quantize(*bytes, quantum).min(reachable))
        })
        .filter(|(_, quanta)| *quanta > 0)
        .collect();
    if need.is_empty() {
        return Selection { chosen: Vec::new(), method: if emergency { Method::Emergency } else { Method::Milp }, solver_error: None };
    }
    if emergency {
        return Selection { chosen: greedy(units, &open, &edges, &need, quantum), method: Method::Emergency, solver_error: None };
    }
    match milp(units, &open, &edges, &need, quantum) {
        Ok(chosen) => Selection { chosen, method: Method::Milp, solver_error: None },
        Err(error) => Selection {
            chosen: greedy(units, &open, &edges, &need, quantum),
            method: Method::SolverFallback,
            solver_error: Some(error.to_string()),
        },
    }
}

/// A handed-over unit costs this fraction of its regret in the objective: it
/// wins every tie and near-tie, yet never outweighs a real difference in harm.
const HANDED_DISCOUNT: f64 = 1e-3;

fn objective_weight(unit: &Unit) -> f64 {
    if unit.handed {
        unit.regret * HANDED_DISCOUNT
    } else {
        unit.regret
    }
}

fn milp(
    units: &[Unit],
    open: &[bool],
    edges: &[(usize, usize)],
    need: &BTreeMap<&str, u64>,
    quantum: u64,
) -> Result<Vec<usize>, good_lp::ResolutionError> {
    let mut problem = ProblemVariables::new();
    // Open units are exactly those on a volume with a target (see [`chains`]).
    let vars: HashMap<usize, Variable> =
        (0..units.len()).filter(|&index| open[index]).map(|index| (index, problem.add(variable().binary()))).collect();
    let objective: Expression = vars.iter().map(|(&index, &x)| objective_weight(&units[index]) * x).sum();
    let mut model = problem.minimise(objective).using(highs);
    model.set_verbose(false);
    model = model.set_time_limit(SOLVER_TIME_LIMIT_SECS);
    for (volume, quanta) in need {
        let covered: Expression = vars
            .iter()
            .filter(|(&index, _)| units[index].volume == *volume)
            .map(|(&index, &x)| quantize(units[index].size_bytes, quantum) as f64 * x)
            .sum();
        model = model.with(constraint!(covered >= *quanta as f64));
    }
    // Edges join open units only, so both ends are variables.
    for (dependent, prerequisite) in edges {
        if let (Some(&a), Some(&b)) = (vars.get(dependent), vars.get(prerequisite)) {
            model = model.with(constraint!(a <= b));
        }
    }
    let solution = model.solve()?;
    let mut chosen: Vec<usize> = vars.iter().filter(|(_, &x)| solution.value(x) > 0.5).map(|(&index, _)| index).collect();
    chosen.sort_unstable();
    Ok(chosen)
}

/// HiGHS gets this long. A 5,000-unit program with one row per volume solves
/// in well under a second; the limit bounds a pathological one.
const SOLVER_TIME_LIMIT_SECS: f64 = 30.0;

/// Bytes per unit of regret, with ε so a zero-regret unit ranks first instead
/// of dividing by zero.
fn efficiency(unit: &Unit) -> f64 {
    const EPSILON: f64 = 1e-9;
    unit.size_bytes as f64 / (objective_weight(unit) + EPSILON)
}

struct Ready {
    index: usize,
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
    /// Max-heap order: handed first, then most bytes per regret, then the
    /// lower index, so the pass is reproducible.
    fn cmp(&self, other: &Self) -> Ordering {
        self.handed.cmp(&other.handed).then(self.efficiency.total_cmp(&other.efficiency)).then(other.index.cmp(&self.index))
    }
}

/// What [`release`] does with the best ready unit.
enum Step {
    Take,
    Skip,
    Stop,
}

/// Walk `open` units in precedence order, best ready first: a unit is ready
/// once its prerequisite was taken. Returns each taken unit with its
/// prerequisite. O(N log N).
fn release(units: &[Unit], open: &[bool], edges: &[(usize, usize)], mut step: impl FnMut(usize) -> Step) -> Vec<(usize, Option<usize>)> {
    // Each unit has at most one prerequisite (chains), and unlocks at most one.
    let mut blocked_by: HashMap<usize, usize> = HashMap::new();
    let mut unlocks: HashMap<usize, usize> = HashMap::new();
    for &(dependent, prerequisite) in edges {
        blocked_by.insert(dependent, prerequisite);
        unlocks.insert(prerequisite, dependent);
    }
    let ready = |index: usize| Ready { index, handed: units[index].handed, efficiency: efficiency(&units[index]) };
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

/// Greedy cover: the best ready unit on a volume still short of its target.
fn greedy(units: &[Unit], open: &[bool], edges: &[(usize, usize)], need: &BTreeMap<&str, u64>, quantum: u64) -> Vec<usize> {
    let mut covered: BTreeMap<&str, u64> = BTreeMap::new();
    let short =
        |covered: &BTreeMap<&str, u64>, volume: &str| covered.get(volume).copied().unwrap_or(0) < need.get(volume).copied().unwrap_or(0);
    let mut chosen: Vec<usize> = release(units, open, edges, |index| {
        if need.keys().all(|volume| !short(&covered, volume)) {
            return Step::Stop;
        }
        let volume = units[index].volume.as_str();
        if !short(&covered, volume) {
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

/// The chosen units in the order they should leave: prerequisites first, then
/// most bytes per regret. Each comes with the chosen unit it must follow, so
/// any prefix of the order (a per-run cap) orphans nothing.
pub fn release_order(units: &[Unit], chosen: &[usize]) -> Vec<(usize, Option<usize>)> {
    let mut open = vec![false; units.len()];
    for &index in chosen {
        open[index] = true;
    }
    let edges = edges(units, &mut open);
    release(units, &open, &edges, |_| Step::Take)
}

/// Every precedence edge the selection breaks, as `(dependent, prerequisite)`
/// pairs: a dependent chosen while its prerequisite was not. Empty for every
/// plan [`select`] returns.
pub fn violations(units: &[Unit], chosen: &[usize]) -> Vec<(usize, usize)> {
    let picked: std::collections::HashSet<usize> = chosen.iter().copied().collect();
    oriented_chains(units)
        .iter()
        .flat_map(|members| members.windows(2))
        .filter(|pair| picked.contains(&pair[0]) && !picked.contains(&pair[1]))
        .map(|pair| (pair[0], pair[1]))
        .collect()
}

#[cfg(test)]
mod tests;
