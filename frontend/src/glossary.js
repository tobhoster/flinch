/**
 * The one source of truth for what FLINCH's terms mean. The inline explainers
 * and the "How FLINCH decides" card both read from here, so they cannot drift.
 */
export const GLOSSARY = {
  p_safe: {
    term: 'P(safe)',
    body: 'The chance that nobody in the household plays the item within the next 30 days, forecast from this household’s own Plex and Tautulli history. Rules are separate: favorites, keep tags and collections, and the newest aired season are kept whatever this number says, and the table marks them with a lock. Calibrated means that of the items shown at 90%, about 9 in 10 really go unplayed; the Forecast model card shows how well that has held up so far.',
  },
  model: {
    term: 'Forecast model',
    body: 'Once a day FLINCH asks your history questions it already knows the answers to: at past dates, was this title played within the next 30 days? Every answer is checked on titles the fit did not see (out of fold). Two fits compete. Recalibrating the priors keeps their order and fits only how sure to be; it needs 40 questions and 2 played titles. The full fit relearns every weight and needs 120 questions and 12 of each outcome. The better one replaces the hand-set priors only when it beats them there. Until then the priors run, and the card says what is still missing.',
  },
  taste: {
    term: 'Genre taste',
    body: 'For a title nobody has played yet, how often this household plays titles of its genres within 30 days, counted from your own history: comedies that got played, horror that sat untouched. A genre seen only a few times leans toward your overall rate until more history comes in. It is learned here, with no outside service, and it starts with no weight: it moves P(safe) only once the full fit adopts it after beating the priors on titles it did not see.',
  },
  score_floor: {
    term: 'Score floor',
    body: 'The minimum P(safe) for an item to be eligible at all. The policy must also allow it: favorites, keep-collections, recently watched items and the newest aired season are never eligible, whatever the score.',
  },
  temperature: {
    term: 'Temperature',
    body: 'Softens overconfident scores before any threshold applies. Above 1 makes them less extreme.',
  },
  eligible: {
    term: 'Eligible and reserve',
    body: 'Items that pass the policy and the floors are eligible. The reserve is their total size: how much space FLINCH could free if it needed to.',
  },
  watermarks: {
    term: 'Watermarks',
    body: 'Below the ceiling (default 80%) nothing is deleted. Crossing it starts eviction on that disk, which continues until use drops to the release mark (default 75%); the gap stops FLINCH deleting one item after every download.',
  },
  eviction_order: {
    term: 'Eviction order',
    body: 'FLINCH frees space with the least expected regret per GiB first: (1 − P(safe)) ÷ size. So a large item that is almost certainly unwanted goes before many small ones.',
  },
  grace_runs: {
    term: 'Grace runs',
    body: 'An item must stay selected for this many consecutive runs before it is handed to Maintainerr, so one odd reading never deletes anything. Dry runs count, so the first enforced run does what the last dry run printed; a run that could not reach Maintainerr does not.',
  },
  never_played: {
    term: 'Never-played reclaim',
    body: 'Lets items nobody ever played become eligible once they have been on disk long enough and pass their own floor. Off by default; “While evicting” arms it only while a disk is over the ceiling.',
  },
  dry_run: {
    term: 'Dry run and Enforced',
    body: 'Dry run scores and plans but sends nothing to Maintainerr. Enforced hands candidates that outlast the grace runs to the Maintainerr collections, and Maintainerr deletes them on its own schedule.',
  },
  evidence: {
    term: 'Watch evidence',
    body: 'Where the “was it watched” evidence came from: Plex play state, Plex show-level state, Tautulli, Plex watch history, or an imported export. With no evidence FLINCH holds the item (fail-closed).',
  },
  held_newest: {
    term: 'Newest aired season',
    body: 'Kept while it is the show’s newest aired season.',
  },
  held_yours: {
    term: 'Kept by you',
    body: 'You marked it to keep: the keep tag on it in Radarr or Sonarr, a Plex label or collection with that name, or your own exclusion in Maintainerr. FLINCH never evicts it.',
  },
  held_no_date: {
    term: 'Watched, no date',
    body: 'Watched, but with no last-played date there is nothing to age it by.',
  },
  held_floor: {
    term: 'Below the floor',
    body: 'P(safe) is under the floor, or the policy does not allow it.',
  },
  held_empty: {
    term: 'Nothing on disk',
    body: 'Monitored, but there is no file, so there is nothing to free.',
  },
  held_excluded: {
    term: 'Excluded',
    body: 'A Maintainerr exclusion is already recorded for it.',
  },
  held_no_evidence: {
    term: 'No watch evidence',
    body: 'Neither Plex nor Tautulli has any record of it this run: not matched by catalogue id, or a source could not be read. FLINCH holds it rather than guess that nobody watched it.',
  },
  held_evidence: {
    term: 'Waiting for complete evidence',
    body: 'Never played, and never-played reclaim would take it, but a watch source was not read completely this run (or Tautulli does not keep every user’s and library’s history), so “never played” cannot be trusted yet.',
  },
  held_reserve: {
    term: 'Eligible reserve',
    body: 'Passes the policy and the floors but is not needed yet. It stays until its disk crosses the ceiling, then goes in regret-per-GiB order until the release mark is reached.',
  },
  not_governed: {
    term: 'Not governed',
    body: 'The item’s root folder is on no disk the *arr apps report, so FLINCH cannot measure what removing it would free and never evicts it. Put the folder on a mount Radarr or Sonarr reports disk space for to govern it.',
  },
  pending: {
    term: 'Waiting for the recycle bin',
    body: 'Evicted files the *arr recycle bin still holds. They count toward the goal, so FLINCH does not evict more to cover space that is already on its way out.',
  },
  evidence_complete: {
    term: 'Complete watch evidence',
    body: 'Never-played reclaim only runs when every configured watch source was read completely this run, and Tautulli keeps history for every active user and for every library FLINCH manages. If one is missing, incomplete or not kept, “never played” cannot be trusted, so those items are held.',
  },
  maintainerr: {
    term: 'Maintainerr sync',
    body: 'FLINCH protects keepers with Maintainerr exclusions and hands eviction candidates to its collections; Maintainerr does the deleting. Each write is checked afterwards, and a failed one is planned again next run.',
  },
  leaving_soon: {
    term: 'Leaving Soon',
    body: 'Items nobody finished go here first: a Maintainerr collection Plex shows on the home screen, which deletes an item only after its window (14 days is a good start). Play one during the window and FLINCH takes it back. Watched items and duplicates skip it and go straight to the delete collections. While the collection is missing, hidden from Plex or has no window, unwatched items are held, never deleted without a warning.',
  },
  unresolved: {
    term: 'Not matched by id',
    body: 'FLINCH finds an item in Plex by its catalogue id (TMDB, TVDB or IMDb), never by title, so a look-alike can never be touched by mistake. An item with no id match is neither protected nor scheduled.',
  },
  operator_keeps: {
    term: 'Your own exclusions',
    body: 'Items you excluded in Maintainerr yourself are kept like favorites. FLINCH never schedules them and never removes an exclusion it did not create.',
  },
  quality_tier: {
    term: 'Quality tier (advice)',
    body: 'Recyclarr defines a premium and a compact profile; FLINCH advises which one an item deserves from the same calibrated evidence (P(safe) at least 85% → compact, below 40% → premium, guards and keeps always premium). It never changes profiles on its own.',
  },
};

/** Card layout: heading, then the glossary keys in reading order. */
export const GLOSSARY_SECTIONS = [
  ['Scoring', ['p_safe', 'model', 'taste', 'score_floor', 'temperature']],
  ['Freeing space', ['eligible', 'watermarks', 'eviction_order', 'pending', 'grace_runs', 'never_played', 'dry_run']],
  ['Evidence', ['evidence', 'evidence_complete']],
  ['Maintainerr', ['maintainerr', 'leaving_soon', 'unresolved', 'operator_keeps']],
  ['Quality', ['quality_tier']],
  ['Why items are held', ['held_reserve', 'not_governed', 'held_no_evidence', 'held_evidence', 'held_yours', 'held_newest', 'held_no_date', 'held_floor', 'held_empty', 'held_excluded']],
];
