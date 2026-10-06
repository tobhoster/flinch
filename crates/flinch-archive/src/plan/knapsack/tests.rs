use super::{quantize, select, violations, Method, Sequence, Unit};
use proptest::prelude::*;
use std::collections::BTreeMap;

const GIB: u64 = 1 << 30;
const Q: u64 = 100 * 1024 * 1024;

fn unit(volume: &str, gib: u64, regret: f64) -> Unit {
    Unit { size_bytes: gib * GIB, regret, volume: volume.to_string(), sequence: None, selectable: true, handed: false }
}

fn season(show: &str, index: u32, played: bool, gib: u64, regret: f64) -> Unit {
    Unit { sequence: Some(Sequence { group: show.to_string(), index, played }), ..unit("tv", gib, regret) }
}

fn targets(pairs: &[(&str, u64)]) -> BTreeMap<String, u64> {
    pairs.iter().map(|(volume, bytes)| (volume.to_string(), *bytes)).collect()
}

fn bytes_on(units: &[Unit], chosen: &[usize], volume: &str) -> u64 {
    chosen.iter().filter(|&&index| units[index].volume == volume).map(|&index| units[index].size_bytes).sum()
}

/// Need 10 GiB. Bytes-per-regret ranks the 9 GiB item first (9/GiB), which
/// then needs the 10 GiB one too; the cheapest cover is the 10 GiB item alone.
fn ratio_trap() -> [Unit; 3] {
    [unit("m", 9, 1.0), unit("m", 10, 1.5), unit("m", 1, 1.0)]
}

#[test]
fn the_solver_takes_the_cheapest_cover_not_the_best_ratio() {
    let units = ratio_trap();
    let selection = select(&units, &targets(&[("m", 10 * GIB)]), Q, false);
    assert_eq!(selection.method, Method::Milp);
    assert_eq!(selection.chosen, vec![1]);
}

#[test]
fn a_volume_with_no_target_gives_up_nothing() {
    let units = [unit("movies", 50, 0.0), unit("tv", 5, 1.0)];
    let selection = select(&units, &targets(&[("tv", 4 * GIB)]), Q, false);
    assert_eq!(selection.chosen, vec![1], "freeing the movies disk relieves nothing on tv");
}

#[test]
fn a_target_beyond_reach_takes_everything_selectable_instead_of_failing() {
    let mut pinned = unit("m", 100, 0.0);
    pinned.selectable = false;
    let units = [unit("m", 3, 1.0), unit("m", 2, 1.0), pinned];
    let selection = select(&units, &targets(&[("m", 500 * GIB)]), Q, false);
    assert_eq!(selection.method, Method::Milp);
    assert_eq!(selection.chosen, vec![0, 1]);
}

#[test]
fn unplayed_seasons_go_from_the_end_even_when_the_start_is_cheaper() {
    // S1 has the least regret, but evicting it before S2 and S3 would leave a
    // show nobody started without its beginning.
    let units = [season("Andor", 1, false, 10, 0.1), season("Andor", 2, false, 10, 5.0), season("Andor", 3, false, 10, 5.0)];
    let selection = select(&units, &targets(&[("tv", 10 * GIB)]), Q, false);
    assert_eq!(selection.chosen, vec![2]);
    assert!(violations(&units, &selection.chosen).is_empty());
}

#[test]
fn played_seasons_go_from_the_start_even_when_the_end_is_cheaper() {
    let units = [season("Lioness", 1, true, 10, 5.0), season("Lioness", 2, true, 10, 0.1)];
    let selection = select(&units, &targets(&[("tv", 10 * GIB)]), Q, false);
    assert_eq!(selection.chosen, vec![0]);
}

#[test]
fn an_excluded_season_protects_every_season_that_depends_on_it() {
    // Unplayed S3 is pinned: S1 and S2 may not go before it, so the only
    // eligible bytes on tv are the unrelated film.
    let mut s3 = season("Andor", 3, false, 10, 0.0);
    s3.selectable = false;
    let units = [season("Andor", 1, false, 10, 0.0), season("Andor", 2, false, 10, 0.0), s3, unit("tv", 4, 9.0)];
    let selection = select(&units, &targets(&[("tv", 30 * GIB)]), Q, false);
    assert_eq!(selection.chosen, vec![3]);
}

#[test]
fn a_season_whose_prerequisite_sits_on_an_untargeted_volume_stays() {
    // S2 lives on tv-b, which needs nothing, so unplayed S1 on tv-a may not go.
    let mut s2 = season("Andor", 2, false, 10, 0.0);
    s2.volume = "tv-b".to_string();
    let mut s1 = season("Andor", 1, false, 10, 0.0);
    s1.volume = "tv-a".to_string();
    let mut film = unit("tv-a", 3, 4.0);
    film.volume = "tv-a".to_string();
    let units = [s1, s2, film];
    for emergency in [false, true] {
        let selection = select(&units, &targets(&[("tv-a", 10 * GIB)]), Q, emergency);
        assert_eq!(selection.chosen, vec![2], "emergency={emergency}");
    }
}

#[test]
fn a_handed_item_wins_a_tie() {
    let mut handed = unit("m", 10, 1.0);
    handed.handed = true;
    let units = [unit("m", 10, 1.0), handed];
    for emergency in [false, true] {
        assert_eq!(select(&units, &targets(&[("m", 10 * GIB)]), Q, emergency).chosen, vec![1], "emergency={emergency}");
    }
}

#[test]
fn emergency_skips_the_solver_and_still_covers_the_target() {
    let units = ratio_trap();
    let selection = select(&units, &targets(&[("m", 10 * GIB)]), Q, true);
    assert_eq!(selection.method, Method::Emergency);
    assert_eq!(selection.chosen, vec![0, 1], "most bytes per regret first, until covered");
    assert!(bytes_on(&units, &selection.chosen, "m") >= 10 * GIB);
}

#[test]
fn quantization_rounds_partial_quanta_up() {
    assert_eq!(quantize(0, Q), 0);
    assert_eq!(quantize(1, Q), 1);
    assert_eq!(quantize(Q, Q), 1);
    assert_eq!(quantize(Q + 1, Q), 2);
}

/// A deterministic library of `n` units: films on two disks and shows of 1-8
/// seasons, a mix of played and unplayed, some pinned.
fn synthetic(n: usize) -> Vec<Unit> {
    let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let mut units = Vec::with_capacity(n);
    while units.len() < n {
        let roll = next();
        if roll % 3 == 0 {
            let volume = if roll % 2 == 0 { "movies" } else { "tv" };
            units.push(Unit { selectable: roll % 17 != 0, ..unit(volume, 1 + next() % 60, (next() % 10_000) as f64 / 1_000.0) });
        } else {
            let show = format!("show-{}", units.len());
            let seasons = 1 + (next() % 8) as u32;
            for index in 1..=seasons {
                if units.len() == n {
                    break;
                }
                let mut s = season(&show, index, next() % 2 == 0, 1 + next() % 40, (next() % 10_000) as f64 / 1_000.0);
                s.selectable = next() % 19 != 0;
                units.push(s);
            }
        }
    }
    units
}

#[test]
fn five_thousand_items_solve_to_a_feasible_ordered_plan() {
    let units = synthetic(5_000);
    let want = targets(&[("movies", 2_000 * GIB), ("tv", 5_000 * GIB)]);
    let started = std::time::Instant::now();
    let selection = select(&units, &want, Q, false);
    let elapsed = started.elapsed();

    assert_eq!(selection.method, Method::Milp, "{:?}", selection.solver_error);
    assert!(violations(&units, &selection.chosen).is_empty());
    for (volume, bytes) in &want {
        // Quantized sizes round up, so real bytes may fall short by under one
        // quantum per chosen unit.
        let slack = selection.chosen.len() as u64 * Q;
        assert!(bytes_on(&units, &selection.chosen, volume) + slack >= *bytes, "{volume} covered");
    }
    assert!(elapsed.as_secs() < 30, "solved in {elapsed:?}");

    let greedy = select(&units, &want, Q, true);
    let cost = |chosen: &[usize]| chosen.iter().map(|&index| units[index].regret).sum::<f64>();
    assert!(cost(&selection.chosen) <= cost(&greedy.chosen) + 1e-6, "the exact plan never costs more than the greedy one");
}

fn arb_units() -> impl Strategy<Value = Vec<Unit>> {
    prop::collection::vec((0u32..4, 1u32..6, any::<bool>(), 1u64..30, 0u32..1000, any::<bool>(), 0u8..10), 1..40).prop_map(|rows| {
        rows.into_iter()
            .map(|(show, index, played, gib, regret, two_disks, pin)| {
                let mut s = season(&format!("s{show}"), index, played, gib, f64::from(regret) / 100.0);
                if two_disks && show % 2 == 0 {
                    s.volume = "tv-b".to_string();
                }
                s.selectable = pin != 0;
                s
            })
            .collect()
    })
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]

    #[test]
    fn no_plan_ever_orphans_part_of_a_show(units in arb_units(), tv in 0u64..400, tv_b in 0u64..400, emergency in any::<bool>()) {
        let want = targets(&[("tv", tv * GIB), ("tv-b", tv_b * GIB)]);
        let selection = select(&units, &want, Q, emergency);
        prop_assert!(violations(&units, &selection.chosen).is_empty(), "{:?}", violations(&units, &selection.chosen));
        prop_assert!(selection.chosen.iter().all(|&index| units[index].selectable), "an excluded unit was chosen");
    }
}
