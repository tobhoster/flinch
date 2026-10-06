/**
 * The one source of truth for what FLINCH's terms mean. The inline explainers
 * and the "How FLINCH decides" card both read from here, so they cannot drift.
 */
export const GLOSSARY = {
  p_watch: {
    term: 'P(watch)',
    body: 'The chance someone in the household plays the item within 90 days, from this household’s Plex and Tautulli history, Seerr requests and watchlists.',
  },
  regret: {
    term: 'Regret',
    body: 'What losing an item is expected to cost: P(watch) × the cost of getting it back × how much the household wants it (Seerr user weights). Low regret goes first.',
  },
  reacquisition: {
    term: 'Reacquisition',
    body: 'How hard the item is to download again. 1 is an ordinary re-download; above 3 is hard (few seeders, usenet out of retention, very large).',
  },
  eviction_safety: {
    term: 'Eviction safety',
    body: 'How safe deleting it is: never above 1 − P(watch), and lower when it is hard to get back.',
  },
  advice: {
    term: 'Recommendation',
    body: 'Keep the release, downgrade to a compact one, or let it go when space is needed. Follows from P(watch) and regret per GiB. Advice only: nothing in Radarr or Sonarr changes.',
  },
  model: {
    term: 'Watch model',
    body: 'A survival hazard: λ₀ per day, scaled by recency, viewings, show plays and season cycle. Once a day FLINCH scores it on past dates whose outcome it knows, using titles the fit did not see. A fitted model replaces the hand-set priors only when it beats them there.',
  },
  projection: {
    term: 'Storage projection',
    body: 'Per disk: used space + average daily downloads × window + queued downloads − evictions already under way. If that would pass the target, the excess plus headroom must be freed. Below the target nothing is planned.',
  },
  plan: {
    term: 'Plan',
    body: 'FLINCH picks the set of items that frees the needed space with the least total regret (a MILP solve). Earlier seasons of a show go before later ones. Above the emergency mark, or if the solver fails, it falls back to a greedy pick by regret per GiB.',
  },
  eligible: {
    term: 'Eligible',
    body: 'Items the plan may pick: not pinned, past the grace period, matched in Plex and on a governed disk. Their total is the most FLINCH could free.',
  },
  pinned: {
    term: 'Pinned',
    body: 'Favorites, keep collections, the keep tag, your own Maintainerr exclusions. Never evicted.',
  },
  grace_period: {
    term: 'Grace period',
    body: 'Items newer than this many days are never picked.',
  },
  grace_runs: {
    term: 'Grace runs',
    body: 'An item must stay picked this many runs in a row before it is handed to Maintainerr. Dry runs count.',
  },
  never_played: {
    term: 'Never-played reclaim',
    body: 'Off by default: items nobody finished (a season nobody played) are kept. It stays off while watch evidence is incomplete or the Leaving Soon title is blank.',
  },
  dry_run: {
    term: 'Dry run',
    body: 'FLINCH plans but sends nothing to Maintainerr. When off, picked items past the grace runs go to the Maintainerr collections, and Maintainerr deletes them on its own schedule.',
  },
  user_weights: {
    term: 'User weights',
    body: 'How much each Seerr user’s requests and watchlist count toward regret. Default 1; 0 ignores the user.',
  },
  evidence: {
    term: 'Watch evidence',
    body: 'Where “was it watched” came from: Plex, Plex show-level state, Tautulli, Plex history or an imported export. Without it the item is kept.',
  },
  evidence_complete: {
    term: 'Complete watch evidence',
    body: 'Never-played reclaim runs only when every watch source was read fully and Tautulli keeps history for every user and library.',
  },
  pending: {
    term: 'Recycle bin',
    body: 'Evicted files the *arr recycle bin still holds. They count as freed, so FLINCH does not evict more for them.',
  },
  held: {
    term: 'Held space',
    body: 'An eviction whose space never came back after the recycle bin window, usually a torrent seeding the same file or a snapshot. FLINCH still counts it as freed for up to 14 days.',
  },
  untracked: {
    term: 'Not library media',
    body: 'Downloads, recycle bins and files no app tracks. Evicting cannot free it; look there first when a disk fills up.',
  },
  not_governed: {
    term: 'Not governed',
    body: 'The root folder is on no disk Radarr or Sonarr reports, so FLINCH cannot measure it and never evicts from it.',
  },
  unresolved: {
    term: 'Not matched in Plex',
    body: 'FLINCH matches Plex by catalogue id (TMDB, TVDB, IMDb), never by title. Without a match an item is neither protected nor evicted.',
  },
  maintainerr: {
    term: 'Maintainerr',
    body: 'FLINCH excludes pinned and partly watched items and hands picked items to Maintainerr collections; Maintainerr deletes. Failed writes are retried next run.',
  },
  leaving_soon: {
    term: 'Leaving Soon',
    body: 'A Maintainerr collection shown on the Plex home screen. Items nobody finished wait there for its window before deletion; playing one takes it back.',
  },
  operator_keeps: {
    term: 'Your own exclusions',
    body: 'Items you excluded in Maintainerr yourself. FLINCH never schedules them or removes those exclusions.',
  },
  released_gone: {
    term: 'Released for gone items',
    body: 'FLINCH removes its own exclusion once the item is gone from both the *arr and a complete Plex listing.',
  },
  outside_deletions: {
    term: 'Deleted outside FLINCH',
    body: 'Movies and seasons Radarr or Sonarr removed in the last 30 days that FLINCH did not hand over. Monitored ones will download again.',
  },
};

/** Card layout: heading, then the glossary keys in reading order. */
export const GLOSSARY_SECTIONS = [
  ['Prediction', ['p_watch', 'regret', 'reacquisition', 'eviction_safety', 'advice', 'model', 'user_weights']],
  ['Freeing space', ['projection', 'plan', 'eligible', 'pinned', 'grace_period', 'grace_runs', 'never_played', 'dry_run']],
  ['Disks', ['pending', 'held', 'untracked', 'not_governed']],
  ['Evidence', ['evidence', 'evidence_complete']],
  ['Maintainerr', ['maintainerr', 'leaving_soon', 'unresolved', 'operator_keeps', 'released_gone', 'outside_deletions']],
];
