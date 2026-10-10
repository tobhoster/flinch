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
//!
//! Operator rules ([`crate::rules`]) add feasibility, never cost: a
//! [`Force::Must`] unit is fixed to 1 whenever its volume has a target (and so
//! is every prerequisite its chain gives it), and on a volume with
//! [`Force::Prefer`] units no other unit is taken until all of them are
//! (`x_other ≤ y_v ≤ x_preferred`). [`Force::Spare`] is the same switch the
//! other way round: a spared unit is taken only once every other unit on its
//! volume is (`x_spared ≤ z_v ≤ x_other`), so it goes only when nothing else
//! there fills the target. The greedy pass ranks the same way: every must-go
//! unit and its prerequisites first, then preferred ones, then the rest, then
//! spared ones; it honours preference only as far as a chain's order allows.
//!
//! The archive tier ([`select_with_moves`], see `moves`) adds a second
//! decision per movie or whole series: move it to an archive volume with room
//! instead of evicting it (`x_i + m_g ≤ 1`), at a near-zero regret.

mod greedy;
mod moves;

use good_lp::{constraint, highs, variable, Expression, ProblemVariables, Solution, SolverModel, Variable};
use greedy::{greedy, release, tiers, Step};
use moves::Usable;
pub use moves::{MoveGroup, Moves, MOVE_REGRET_PER_TIB};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

/// Where a unit sits in its show's order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    /// What an operator rule asks: taken first, or taken whenever its volume
    /// needs space. The caller never sets it on an excluded or protected unit.
    pub force: Option<Force>,
}

/// What an operator rule asks of a unit ([`crate::rules`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Force {
    /// Drawn on before any unit without a rule on its volume, only as far as
    /// the target needs.
    Prefer,
    /// Selected whenever its volume has a target, past the target if need be.
    Must,
    /// Drawn on only once every other unit on its volume is taken: a soft
    /// keep (a torrent below its desired ratio, [`crate::torrents`]).
    Spare,
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
    /// Groups that move to their archive instead ([`Moves::groups`] indices,
    /// ascending); none of their members is in `chosen`.
    pub moved: Vec<usize>,
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
    select_with_moves(units, targets, quantum, emergency, &Moves::default())
}

/// [`select`] with the archive tier: each group in `moves` may move to its
/// archive instead of its members leaving (see [`moves`]).
pub fn select_with_moves(units: &[Unit], targets: &BTreeMap<String, u64>, quantum: u64, emergency: bool, moves: &Moves) -> Selection {
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
        let method = if emergency { Method::Emergency } else { Method::Milp };
        return Selection { chosen: Vec::new(), method, solver_error: None, moved: Vec::new() };
    }
    let usable = moves::usable(units, &open, moves, quantum);
    let fallback = |method, solver_error| {
        let taken = moves::greedy_moves(units, &open, &need, quantum, &usable, moves);
        let rest = moves::remainder(&open, &edges, &need, &usable, &taken);
        let moved = taken.iter().map(|&index| usable[index].group).collect();
        Selection { chosen: greedy(units, &rest.open, &rest.edges, &rest.need, quantum), method, solver_error, moved }
    };
    if emergency {
        return fallback(Method::Emergency, None);
    }
    match milp(units, &open, &edges, &need, quantum, (&usable, moves)) {
        Ok((chosen, moved)) => Selection { chosen, method: Method::Milp, solver_error: None, moved },
        Err(error) => fallback(Method::SolverFallback, Some(error.to_string())),
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
    (usable, moves): (&[Usable], &Moves),
) -> Result<(Vec<usize>, Vec<usize>), good_lp::ResolutionError> {
    let mut problem = ProblemVariables::new();
    // Open units are exactly those on a volume with a target (see [`chains`]).
    let vars: HashMap<usize, Variable> =
        (0..units.len()).filter(|&index| open[index]).map(|index| (index, problem.add(variable().binary()))).collect();
    let m = moves::variables(&mut problem, usable);
    let member_move = moves::by_member(usable, &m);
    // Two switches per volume where rules rank units. Preferred (and must-go)
    // units are taken before the first turns on, which every other unit
    // needs; the mirror for spared units: every other unit is taken before
    // the second turns on, which every spared unit needs. A move handles its
    // members: it waits behind preferred units, and counts as taken for a
    // spared one.
    let mut switches: Vec<(Variable, Vec<Expression>, Vec<Expression>)> = Vec::new();
    for volume in need.keys() {
        let on: Vec<(usize, Variable)> =
            vars.iter().filter(|(&index, _)| units[index].volume == *volume).map(|(&index, &x)| (index, x)).collect();
        let split = |marked: fn(Option<Force>) -> bool| {
            let (hit, rest): (Vec<_>, Vec<_>) = on.iter().copied().partition(|(index, _)| marked(units[*index].force));
            (!hit.is_empty() && !rest.is_empty()).then_some((hit, rest))
        };
        let plain = |pairs: Vec<(usize, Variable)>| pairs.into_iter().map(|(_, x)| Expression::from(x)).collect::<Vec<_>>();
        if let Some((preferred, rest)) = split(|force| matches!(force, Some(Force::Prefer | Force::Must))) {
            let moved_here = usable.iter().zip(&m).filter(|(group, _)| group.volume == *volume).map(|(_, &mv)| Expression::from(mv));
            let after = plain(rest).into_iter().chain(moved_here).collect();
            switches.push((problem.add(variable().binary()), plain(preferred), after));
        }
        if let Some((spared, rest)) = split(|force| force == Some(Force::Spare)) {
            let handled =
                rest.into_iter().map(|(index, x)| member_move.get(&index).map_or_else(|| Expression::from(x), |&mv| x + mv)).collect();
            switches.push((problem.add(variable().binary()), handled, plain(spared)));
        }
    }
    let evictions: Expression = vars.iter().map(|(&index, &x)| objective_weight(&units[index]) * x).sum();
    let copies: Expression = usable.iter().zip(&m).map(|(group, &mv)| group.cost * mv).sum();
    let mut model = problem.minimise(evictions + copies).using(highs);
    model.set_verbose(false);
    model = model.set_time_limit(SOLVER_TIME_LIMIT_SECS);
    for (volume, quanta) in need {
        let evicted = vars
            .iter()
            .filter(|(&index, _)| units[index].volume == *volume)
            .map(|(&index, &x)| quantize(units[index].size_bytes, quantum) as f64 * x);
        let moved = usable.iter().zip(&m).filter(|(group, _)| group.volume == *volume).map(|(group, &mv)| group.quanta as f64 * mv);
        let covered: Expression = evicted.chain(moved).sum();
        model = model.with(constraint!(covered >= *quanta as f64));
    }
    model = moves::constrain(model, &vars, usable, &m, moves, quantum);
    // Edges join open units only, so both ends are variables.
    for (dependent, prerequisite) in edges {
        if let (Some(&a), Some(&b)) = (vars.get(dependent), vars.get(prerequisite)) {
            model = model.with(constraint!(a <= b));
        }
    }
    // Taking everything open meets every row, so these keep the program feasible.
    for (&index, &x) in &vars {
        if units[index].force == Some(Force::Must) {
            model = model.with(constraint!(x >= 1));
        }
    }
    for (switch, before, after) in switches {
        for x in before {
            model = model.with(constraint!(switch <= x));
        }
        for x in after {
            model = model.with(constraint!(x <= switch));
        }
    }
    let solution = model.solve()?;
    let mut chosen: Vec<usize> = vars.iter().filter(|(_, &x)| solution.value(x) > 0.5).map(|(&index, _)| index).collect();
    chosen.sort_unstable();
    let mut moved: Vec<usize> = usable.iter().zip(&m).filter(|(_, &mv)| solution.value(mv) > 0.5).map(|(group, _)| group.group).collect();
    moved.sort_unstable();
    Ok((chosen, moved))
}

/// HiGHS gets this long. A 5,000-unit program with one row per volume solves
/// in well under a second; the limit bounds a pathological one.
const SOLVER_TIME_LIMIT_SECS: f64 = 30.0;

/// The chosen units in the order they should leave: prerequisites first, then
/// most bytes per regret. Each comes with the chosen unit it must follow, so
/// any prefix of the order (a per-run cap) orphans nothing.
pub fn release_order(units: &[Unit], chosen: &[usize]) -> Vec<(usize, Option<usize>)> {
    let mut open = vec![false; units.len()];
    for &index in chosen {
        open[index] = true;
    }
    let edges = edges(units, &mut open);
    let tier = tiers(units, &open, &edges);
    release(units, &open, &edges, &tier, |_| Step::Take)
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
