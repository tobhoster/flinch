//! End-to-end fixture: the plan the shipped CLI produces for the checked-in
//! `fixtures/arr/cards.json` card set.
//!
//! Hand-audited expectations (retention 90, dedupe movies, keep newest season):
//! delete s01 (estate 1.8G), s05 (1.6G), s06 (2.3G), m01 (4.2G), m03 dup (6.2G);
//! keep the other seven. If this test fails, the plan and the documented policy
//! disagree — one of them is wrong.

use flinch_archive::card::ArchiveCard;
use flinch_archive::plan::{build_plan, Baseline, ReclaimGoal};
use flinch_archive::ArchivePolicy;

const CARDS: &str = include_str!("../../../fixtures/arr/cards.json");

#[test]
fn the_shipped_fixture_produces_the_hand_audited_plan() {
    let cards: Vec<ArchiveCard> = serde_json::from_str(CARDS).expect("fixture parses");
    let policy = ArchivePolicy::default();
    let model = Baseline::new(policy);
    let plan = build_plan(&cards, &model, &policy, 0.95, &std::collections::HashMap::new(), &ReclaimGoal::AllSafe);

    let mut deleted: Vec<&str> = plan.entries.iter().map(|e| e.id.as_str()).collect();
    deleted.sort_unstable();

    let expected = vec!["m01", "m03", "s01", "s05", "s06"];
    assert_eq!(deleted, expected, "the plan must match the hand audit exactly");

    let expected_bytes = 1_800_000_000_u64 + 1_600_000_000 + 2_300_000_000 + 4_200_000_000 + 6_200_000_000;
    assert_eq!(plan.reclaimed_bytes, expected_bytes, "reclaimed bytes must match");
}
