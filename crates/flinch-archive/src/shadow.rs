//! What arming the never-played rule would add, computed without acting on it.
//!
//! The operator sees this number before flipping the switch, so it has to be
//! right. It is a library function (not inline in the daemon) because the first
//! version paired cards with `verdicts.values()` — a `HashMap`, iterated in
//! arbitrary order — and reported a different, meaningless count every run.
//! Verdicts are looked up by card id here, and the test below pins that.

use crate::card::ArchiveCard;
use crate::policy::{self, ArchivePolicy, ScoreVerdict, UnwatchedReclaim};
use std::collections::HashMap;

/// Items and bytes that would *additionally* qualify with the rule armed.
pub fn preview(
    cards: &[ArchiveCard],
    verdicts: &HashMap<String, ScoreVerdict>,
    policy: &ArchivePolicy,
) -> (usize, u64) {
    let armed = ArchivePolicy {
        unwatched_reclaim: UnwatchedReclaim { enabled: true, ..policy.unwatched_reclaim },
        ..policy.clone()
    };
    let mut count = 0usize;
    let mut bytes = 0u64;
    for card in cards {
        let Some(verdict) = verdicts.get(&card.id).copied() else { continue };
        let already = policy::reclaims_bytes(&policy::decide(card, policy, Some(verdict))) > 0;
        let with_rule = policy::reclaims_bytes(&policy::decide(card, &armed, Some(verdict))) > 0;
        if with_rule && !already {
            count += 1;
            bytes += card.size_bytes;
        }
    }
    (count, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::card::SeasonState;

    fn unplayed_season(id: &str, size: u64) -> ArchiveCard {
        let mut card = crate::golden::golden_season();
        card.id = id.to_string();
        card.season_state = Some(SeasonState::Empty);
        card.is_newest_season = Some(false);
        card.last_watched_days = None;
        card.added_days_ago = 300.0;
        card.size_bytes = size;
        card
    }

    #[test]
    fn each_card_is_judged_by_its_own_verdict() {
        // Two items, deliberately different verdicts. Pairing by iteration order
        // instead of id would swap them and report the wrong item.
        let cards = [unplayed_season("keep-me", 10), unplayed_season("reclaim-me", 70)];
        let mut verdicts = HashMap::new();
        verdicts.insert("keep-me".to_string(), ScoreVerdict { p_safe: 0.20, hard_guard: false, sibling_played: false });
        verdicts.insert("reclaim-me".to_string(), ScoreVerdict { p_safe: 0.90, hard_guard: false, sibling_played: false });

        assert_eq!(preview(&cards, &verdicts, &ArchivePolicy::default()), (1, 70));

        // Same data, reversed card order: the answer cannot depend on it.
        let reversed = [cards[1].clone(), cards[0].clone()];
        assert_eq!(preview(&reversed, &verdicts, &ArchivePolicy::default()), (1, 70));
    }

    #[test]
    fn items_the_policy_already_reclaims_are_not_counted_twice() {
        let mut armed = ArchivePolicy::default();
        armed.unwatched_reclaim.enabled = true;
        let cards = [unplayed_season("already", 50)];
        let mut verdicts = HashMap::new();
        verdicts.insert("already".to_string(), ScoreVerdict { p_safe: 0.95, hard_guard: false, sibling_played: false });
        // With the rule already armed, arming it again adds nothing.
        assert_eq!(preview(&cards, &verdicts, &armed), (0, 0));
    }

    #[test]
    fn an_item_without_a_verdict_is_skipped_not_guessed() {
        let cards = [unplayed_season("orphan", 50)];
        assert_eq!(preview(&cards, &HashMap::new(), &ArchivePolicy::default()), (0, 0));
    }
}
