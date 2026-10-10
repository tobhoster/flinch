use super::super::{quantize, select_with_moves, violations, Force, Sequence, Unit};
use super::{MoveGroup, Moves};
use proptest::prelude::*;
use rstest::rstest;
use std::collections::BTreeMap;

const GIB: u64 = 1 << 30;
const Q: u64 = 100 * 1024 * 1024;

fn unit(volume: &str, gib: u64, regret: f64) -> Unit {
    Unit { size_bytes: gib * GIB, regret, volume: volume.to_string(), sequence: None, selectable: true, handed: false, force: None }
}

fn season(show: &str, index: u32, gib: u64, regret: f64) -> Unit {
    Unit { sequence: Some(Sequence { group: show.to_string(), index, played: false }), ..unit("tv", gib, regret) }
}

fn forced(unit: Unit, force: Force) -> Unit {
    Unit { force: Some(force), ..unit }
}

fn targets(pairs: &[(&str, u64)]) -> BTreeMap<String, u64> {
    pairs.iter().map(|(volume, gib)| (volume.to_string(), gib * GIB)).collect()
}

/// Every group archives to one disk with `room_gib` of headroom.
fn archive(groups: &[&[usize]], room_gib: u64) -> Moves {
    Moves {
        groups: groups.iter().map(|members| MoveGroup { members: members.to_vec(), archive: "archive".to_string() }).collect(),
        headroom: BTreeMap::from([("archive".to_string(), room_gib * GIB)]),
    }
}

#[rstest]
#[case::milp(false)]
#[case::greedy(true)]
fn with_room_on_the_archive_a_move_replaces_the_eviction(#[case] emergency: bool) {
    let units = [unit("m", 10, 0.5), unit("m", 10, 2.0)];
    let selection = select_with_moves(&units, &targets(&[("m", 10)]), Q, emergency, &archive(&[&[0], &[1]], 100));
    assert!(selection.chosen.is_empty(), "nothing is deleted while the archive has room");
    assert_eq!(selection.moved.len(), 1, "one move covers the target");
}

#[rstest]
#[case::milp_too_small(false, 5)]
#[case::greedy_too_small(true, 5)]
#[case::milp_full(false, 0)]
#[case::greedy_full(true, 0)]
fn without_room_on_the_archive_the_plan_falls_back_to_evicting(#[case] emergency: bool, #[case] room_gib: u64) {
    let units = [unit("m", 10, 0.5), unit("m", 10, 2.0)];
    let selection = select_with_moves(&units, &targets(&[("m", 10)]), Q, emergency, &archive(&[&[0], &[1]], room_gib));
    assert_eq!((selection.chosen, selection.moved), (vec![0], vec![]));
}

#[rstest]
#[case::milp(false)]
#[case::greedy(true)]
fn with_room_for_one_the_archive_saves_a_dear_item_and_the_cheapest_goes(#[case] emergency: bool) {
    // Room for one of the three (sizes round up, room rounds down: 10 GiB is
    // 103 quanta, 15 GiB of room 153); the target needs two.
    let units = [unit("m", 10, 0.5), unit("m", 10, 2.0), unit("m", 10, 1.0)];
    let selection = select_with_moves(&units, &targets(&[("m", 20)]), Q, emergency, &archive(&[&[0], &[1], &[2]], 15));
    assert_eq!(selection.chosen, [0]);
    assert!(selection.moved == [1] || selection.moved == [2], "one dear item moves: {:?}", selection.moved);
}

#[rstest]
#[case::milp(false)]
#[case::greedy(true)]
fn a_series_moves_whole_or_not_at_all(#[case] emergency: bool) {
    let units = [season("Andor", 1, 10, 1.0), season("Andor", 2, 10, 1.0), season("Andor", 3, 10, 1.0)];
    let group: &[usize] = &[0, 1, 2];

    let roomy = select_with_moves(&units, &targets(&[("tv", 10)]), Q, emergency, &archive(&[group], 100));
    assert_eq!((roomy.chosen, roomy.moved), (vec![], vec![0]), "the whole series moves, past what the target needs");

    // 20 GiB would take two seasons, but Sonarr moves the series or nothing.
    let tight = select_with_moves(&units, &targets(&[("tv", 10)]), Q, emergency, &archive(&[group], 20));
    assert_eq!((tight.chosen, tight.moved), (vec![2], vec![]), "seasons still evict one by one, from the end");
}

#[rstest]
#[case::milp(false)]
#[case::greedy(true)]
fn an_eviction_cheaper_than_the_copy_still_goes(#[case] emergency: bool) {
    let units = [unit("m", 10, 0.0)];
    let selection = select_with_moves(&units, &targets(&[("m", 10)]), Q, emergency, &archive(&[&[0]], 100));
    assert_eq!((selection.chosen, selection.moved), (vec![0], vec![]));
}

#[rstest]
#[case::milp(false)]
#[case::greedy(true)]
fn rules_rank_moves_like_evictions(#[case] emergency: bool) {
    // A preferred unit still goes first; a spared one waits behind a move.
    let preferred = [forced(unit("m", 10, 3.0), Force::Prefer), unit("m", 10, 1.0)];
    let selection = select_with_moves(&preferred, &targets(&[("m", 10)]), Q, emergency, &archive(&[&[1]], 100));
    assert_eq!((selection.chosen, selection.moved), (vec![0], vec![]));

    let spared = [forced(unit("m", 10, 0.0), Force::Spare), unit("m", 10, 1.0)];
    let selection = select_with_moves(&spared, &targets(&[("m", 10)]), Q, emergency, &archive(&[&[1]], 100));
    assert_eq!((selection.chosen, selection.moved), (vec![], vec![0]));
}

#[test]
fn a_group_already_on_its_archive_volume_never_moves() {
    let units = [unit("archive", 10, 2.0)];
    let selection = select_with_moves(&units, &targets(&[("archive", 10)]), Q, false, &archive(&[&[0]], 100));
    assert_eq!((selection.chosen, selection.moved), (vec![0], vec![]));
}

/// Shows of one to three seasons, each show one move group.
fn arb_library() -> impl Strategy<Value = (Vec<Unit>, Vec<Vec<usize>>)> {
    prop::collection::vec(prop::collection::vec((1u64..30, 0u32..1000), 1..4), 1..12).prop_map(|shows| {
        let (mut units, mut groups) = (Vec::new(), Vec::new());
        for (show, seasons) in shows.into_iter().enumerate() {
            let start = units.len();
            for (index, (gib, regret)) in seasons.into_iter().enumerate() {
                units.push(season(&format!("s{show}"), index as u32 + 1, gib, f64::from(regret) / 100.0));
            }
            groups.push((start..units.len()).collect());
        }
        (units, groups)
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]
    #[test]
    fn moves_respect_the_room_never_overlap_evictions_and_cover_the_target(
        (units, groups) in arb_library(),
        target_gib in 1u64..200,
        room_gib in 0u64..150,
        emergency in any::<bool>(),
    ) {
        let members: Vec<&[usize]> = groups.iter().map(Vec::as_slice).collect();
        let moves = archive(&members, room_gib);
        let selection = select_with_moves(&units, &targets(&[("tv", target_gib)]), Q, emergency, &moves);
        let moved: Vec<usize> = selection.moved.iter().flat_map(|&group| groups[group].iter().copied()).collect();
        let quanta = |indices: &[usize]| indices.iter().map(|&index| quantize(units[index].size_bytes, Q)).sum::<u64>();
        prop_assert!(quanta(&moved) <= room_gib * GIB / Q, "moves fit the archive's headroom");
        prop_assert!(moved.iter().all(|index| !selection.chosen.contains(index)), "x + m ≤ 1");
        prop_assert!(violations(&units, &selection.chosen).is_empty());
        let reachable = quanta(&(0..units.len()).collect::<Vec<_>>());
        prop_assert!(quanta(&selection.chosen) + quanta(&moved) >= quantize(target_gib * GIB, Q).min(reachable));
    }
}
