//! The archive tier's decision next to eviction ([`crate::archive`]): a group
//! of units — a movie, or every season of a series, since Sonarr moves whole
//! series only — may move to an archive volume instead of leaving.
//!
//! ```text
//! min  Σ R_i x_i + Σ c_g m_g
//! s.t. Σ_{i on v} Ŝ_i x_i + Σ_{g on v} Ŝ_g m_g ≥ B̂_v
//!      x_i + m_g ≤ 1                  for every member i of g
//!      Σ_{g to a} Ŝ_g m_g ≤ H_a        for every archive volume a
//! ```
//!
//! `c_g` is near zero: a moved item stays playable, so its regret is only the
//! copy's IO ([`MOVE_REGRET_PER_TIB`]). The solver therefore archives before
//! it evicts while the archive has room (`H_a`, quantized down, from that
//! volume's own forecast), and spends the room on the groups whose eviction
//! would hurt most; an item whose eviction regret is below the IO term still
//! goes. A group is usable only when every member is open, none carries a
//! rule, it sits on one volume other than its archive, and it fits the room at
//! all. Rule switches see a move as "this unit is handled": preferred units
//! still go before any move, and spared units only once every other unit on
//! their volume is evicted or moved.
//!
//! The greedy pass mirrors it: must-go and preferred units count first, then
//! moves by eviction regret per quantum while the archive has room, then the
//! usual eviction greedy for what is left.

use super::{objective_weight, quantize, Force, Unit};
use good_lp::{constraint, variable, Expression, ProblemVariables, SolverModel, Variable};
use std::collections::{BTreeMap, HashMap};

/// Units the *arr moves together to one archive volume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoveGroup {
    /// Indices into the unit slice.
    pub members: Vec<usize>,
    /// The volume holding the archive root.
    pub archive: String,
}

/// The archive tier as the solver sees it. Empty: evictions only.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Moves {
    pub groups: Vec<MoveGroup>,
    /// Archive volume → bytes it can take while its forecast stays under target.
    pub headroom: BTreeMap<String, u64>,
}

/// The regret of moving one TiB: the copy's IO only, small enough that every
/// eviction with real regret costs more, large enough to prefer copying less.
pub const MOVE_REGRET_PER_TIB: f64 = 1e-3;

const TIB: f64 = (1u64 << 40) as f64;

/// A group the solver may move this run.
pub(super) struct Usable<'a> {
    /// Index into [`Moves::groups`].
    pub group: usize,
    pub members: &'a [usize],
    pub volume: &'a str,
    pub archive: &'a str,
    pub quanta: u64,
    /// The objective weight of evicting every member instead.
    pub avoided: f64,
    /// The objective weight of the move.
    pub cost: f64,
}

/// Whole quanta the archive volume can take.
fn room(moves: &Moves, archive: &str, quantum: u64) -> u64 {
    moves.headroom.get(archive).map_or(0, |bytes| bytes / quantum.max(1))
}

/// The groups that may move: every member open and unruled, all on one
/// volume that is not the archive, the whole group within the archive's room.
pub(super) fn usable<'a>(units: &'a [Unit], open: &[bool], moves: &'a Moves, quantum: u64) -> Vec<Usable<'a>> {
    moves
        .groups
        .iter()
        .enumerate()
        .filter_map(|(group, candidate)| {
            let volume = units.get(*candidate.members.first()?)?.volume.as_str();
            let fits = candidate
                .members
                .iter()
                .all(|&index| open.get(index).copied().unwrap_or(false) && units[index].force.is_none() && units[index].volume == volume);
            if !fits || volume == candidate.archive {
                return None;
            }
            let quanta: u64 = candidate.members.iter().map(|&index| quantize(units[index].size_bytes, quantum)).sum();
            let bytes = candidate.members.iter().map(|&index| units[index].size_bytes).fold(0u64, u64::saturating_add);
            (quanta > 0 && quanta <= room(moves, &candidate.archive, quantum)).then(|| Usable {
                group,
                members: &candidate.members,
                volume,
                archive: &candidate.archive,
                quanta,
                avoided: candidate.members.iter().map(|&index| objective_weight(&units[index])).sum(),
                cost: bytes as f64 / TIB * MOVE_REGRET_PER_TIB,
            })
        })
        .collect()
}

/// Greedy moves, as indices into `usable`: must-go and preferred units count
/// toward each target first, then the groups whose eviction would hurt most
/// per quantum move while their volume is short and their archive has room.
/// A group cheaper to evict than to copy is left to eviction.
pub(super) fn greedy_moves(
    units: &[Unit],
    open: &[bool],
    need: &BTreeMap<&str, u64>,
    quantum: u64,
    usable: &[Usable],
    moves: &Moves,
) -> Vec<usize> {
    let mut covered: BTreeMap<&str, u64> = BTreeMap::new();
    for (unit, _) in units.iter().zip(open).filter(|(unit, open)| **open && matches!(unit.force, Some(Force::Must | Force::Prefer))) {
        *covered.entry(unit.volume.as_str()).or_insert(0) += quantize(unit.size_bytes, quantum);
    }
    let mut left: BTreeMap<&str, u64> = usable.iter().map(|group| (group.archive, room(moves, group.archive, quantum))).collect();
    let density = |group: &Usable| group.avoided / group.quanta as f64;
    let mut order: Vec<usize> = (0..usable.len()).filter(|&index| usable[index].avoided > usable[index].cost).collect();
    order.sort_by(|&a, &b| density(&usable[b]).total_cmp(&density(&usable[a])).then(a.cmp(&b)));
    let mut taken = Vec::new();
    for index in order {
        let group = &usable[index];
        let short = covered.get(group.volume).copied().unwrap_or(0) < need.get(group.volume).copied().unwrap_or(0);
        let room = left.get(group.archive).copied().unwrap_or(0);
        if short && room >= group.quanta {
            *covered.entry(group.volume).or_insert(0) += group.quanta;
            left.insert(group.archive, room - group.quanta);
            taken.push(index);
        }
    }
    taken.sort_unstable();
    taken
}

/// What is left for eviction once some groups move.
pub(super) struct Remainder<'n> {
    pub open: Vec<bool>,
    pub edges: Vec<(usize, usize)>,
    pub need: BTreeMap<&'n str, u64>,
}

/// The `taken` groups' members closed (whole shows, so no other chain
/// changes), each target less what they cover.
pub(super) fn remainder<'n>(
    open: &[bool],
    edges: &[(usize, usize)],
    need: &BTreeMap<&'n str, u64>,
    usable: &[Usable],
    taken: &[usize],
) -> Remainder<'n> {
    let mut open = open.to_vec();
    let mut need = need.clone();
    for group in taken.iter().map(|&index| &usable[index]) {
        for &member in group.members {
            open[member] = false;
        }
        if let Some(quanta) = need.get_mut(group.volume) {
            *quanta = quanta.saturating_sub(group.quanta);
        }
    }
    need.retain(|_, quanta| *quanta > 0);
    let edges = edges.iter().copied().filter(|(a, b)| open[*a] && open[*b]).collect();
    Remainder { open, edges, need }
}

/// One binary move variable per usable group.
pub(super) fn variables(problem: &mut ProblemVariables, usable: &[Usable]) -> Vec<Variable> {
    usable.iter().map(|_| problem.add(variable().binary())).collect()
}

/// Unit index → its group's move variable.
pub(super) fn by_member(usable: &[Usable], vars: &[Variable]) -> HashMap<usize, Variable> {
    usable.iter().zip(vars).flat_map(|(group, &m)| group.members.iter().map(move |&member| (member, m))).collect()
}

/// `x_i + m_g ≤ 1` for every member, and one room row per archive volume.
pub(super) fn constrain<M: SolverModel>(
    mut model: M,
    x: &HashMap<usize, Variable>,
    usable: &[Usable],
    vars: &[Variable],
    moves: &Moves,
    quantum: u64,
) -> M {
    let mut rows: BTreeMap<&str, Vec<(f64, Variable)>> = BTreeMap::new();
    for (group, &m) in usable.iter().zip(vars) {
        for member in group.members {
            if let Some(&x) = x.get(member) {
                model = model.with(constraint!(x + m <= 1));
            }
        }
        rows.entry(group.archive).or_default().push((group.quanta as f64, m));
    }
    for (archive, terms) in rows {
        let used: Expression = terms.into_iter().map(|(quanta, m)| quanta * m).sum();
        model = model.with(constraint!(used <= room(moves, archive, quantum) as f64));
    }
    model
}

#[cfg(test)]
mod tests;
