//! The answers cache: days that only move forward, and waiting questions
//! capped with the cycle's ahead of the panel's.

use super::*;

const DAY: u64 = 86_400;
const NOW: u64 = 1_800_000_000;

/// Lookups of `count` distinct questions, numbered from `from`.
fn questions(cache: &AnswerCache, from: usize, count: usize) -> Asked {
    let mut judge = Judge::new(cache);
    for n in from..from + count {
        let nearest = vec![Neighbour { title: "Heat (1995)".into(), played: true }];
        judge.p(State { nearest, title: format!("Title: {n}") });
    }
    judge.finish()
}

fn cache() -> AnswerCache {
    let mut cache = AnswerCache::default();
    cache.retarget("clef");
    cache
}

#[test]
fn an_offline_fit_dated_in_the_past_never_ages_an_answer() {
    let mut cache = cache();
    let asked = questions(&cache, 0, 2);
    cache.note(&asked, NOW);
    let queue = cache.queue(&asked);
    cache.answer(queue[0].0.clone(), 0.7, NOW);
    let asked = questions(&cache, 0, 2);
    assert!(!cache.note_panel(&asked, NOW - 20 * DAY), "a past day moves nothing");
    assert!(!cache.prune(NOW + KEEP_DAYS * DAY), "still dated today: the answer and the question stay");
    assert_eq!((cache.answered(), cache.pending()), (1, 1));
    assert!(cache.note(&asked, NOW + DAY), "a later day moves both forward");
}

#[test]
fn waiting_questions_are_capped_and_the_panels_give_way_to_the_cycles() {
    let mut cache = cache();
    cache.note_panel(&questions(&cache, 0, MAX_PENDING + 5), NOW);
    assert_eq!(cache.pending(), MAX_PENDING, "the panel fills the room and no more");
    assert!(!cache.note_panel(&questions(&cache, 5_000, 1), NOW), "a full cache takes no more of the panel's");
    assert!(cache.note(&questions(&cache, 1_000_000, 3), NOW));
    assert_eq!(cache.pending(), MAX_PENDING, "each of the cycle's pushed one of the panel's out");
    let queue = cache.queue(&Asked::default());
    let first: Vec<&str> = queue[..3].iter().map(|(_, state)| state.title.as_str()).collect();
    assert!(first.iter().all(|title| title.starts_with("Title: 100000")), "the cycle's are asked first: {first:?}");
}
