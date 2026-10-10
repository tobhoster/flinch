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
    body: 'How hard the item is to download again. 1 is an ordinary re-download; above 3 is hard (few seeders, usenet out of retention, very large). Streaming on a service you subscribe to lowers it toward the floor.',
  },
  eviction_safety: {
    term: 'Eviction safety',
    body: 'How safe deleting it is: never above 1 − P(watch), and lower when it is hard to get back.',
  },
  advice: {
    term: 'Recommendation',
    body: 'Keep the release, downgrade to a compact one, or let it go when space is needed. Follows from P(watch) and regret per GiB; a large or 2160p file in a seldom-played theme is advised a downgrade unless pinned or partly watched. Advice only, unless Settings → Quality → Downgrades is on.',
  },
  model: {
    term: 'Watch model',
    body: 'A survival hazard: λ₀ per day, scaled by recency, viewings, show plays, season cycle, whether everyone who played it finished it, and — for a title nobody played — how readily the household plays similar titles (taste, from EmbeddingGemma 2 vectors of each title’s description, and of its poster when Settings → Taste embeddings → Posters is on). Once a day FLINCH scores it on past dates whose outcome it knows, using titles the fit did not see. A fitted model replaces the hand-set priors only when it beats them there.',
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
    body: 'Favorites, keep collections, the keep tag, your own Maintainerr exclusions, and titles someone in the household kept with a link (until that keep ends). Never evicted.',
  },
  grace_period: {
    term: 'Grace period',
    body: 'Items newer than this many days are never picked.',
  },
  grace_runs: {
    term: 'Grace runs',
    body: 'An item must stay picked this many runs in a row before it is handed over (to Maintainerr, or to the native executor). Dry runs count.',
  },
  never_played: {
    term: 'Never-played reclaim',
    body: 'Off by default: items nobody finished (a season nobody played) are kept. It stays off while watch evidence is incomplete or the Leaving Soon title is blank.',
  },
  dry_run: {
    term: 'Dry run',
    body: 'FLINCH plans and prints every write it would make, but sends none. When off, picked items past the grace runs go to the Maintainerr collections and Maintainerr deletes them on its own schedule, or, with the native executor, FLINCH announces and deletes them itself.',
  },
  user_weights: {
    term: 'User weights',
    body: 'How much each Seerr user’s requests and watchlist count toward regret. Default 1; 0 ignores the user.',
  },
  evidence: {
    term: 'Watch evidence',
    body: 'Where “was it watched” came from: Plex, Plex show-level state, Tautulli, Plex history, Jellyfin/Emby (every user’s played state), Tracearr or Trakt plays, or an imported export. Without it the item is kept.',
  },
  evidence_complete: {
    term: 'Complete watch evidence',
    body: 'Never-played reclaim runs only when every watch source was read fully (Jellyfin/Emby: every user and every item; Tracearr and Trakt: every account’s whole history), Tautulli keeps history for every user and library, and Plex is connected for Leaving Soon.',
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
    term: 'Not matched in the media server',
    body: 'FLINCH matches Plex (or, when the native executor\'s Leaving Soon shelf is on Jellyfin/Emby, Jellyfin/Emby) by catalogue id (TMDB, TVDB, IMDb), never by title. Without a match an item is neither protected nor evicted.',
  },
  maintainerr: {
    term: 'Maintainerr',
    body: 'FLINCH excludes pinned and partly watched items and hands picked items to Maintainerr collections; Maintainerr deletes. Failed writes are retried next run.',
  },
  leaving_soon: {
    term: 'Leaving Soon',
    body: 'A collection shown on the Plex home screen: Maintainerr’s, or with the native executor one FLINCH keeps in each Plex library, sorted soonest-leaving first with “Leaves between Oct 14 and Oct 23” in its summary (and, if you turn it on, the date badged on each poster). The native executor can keep it on Jellyfin/Emby instead: one collection for the whole server, with no home promotion, so name it to sort first. Items nobody finished wait there for its window before deletion; playing one, on any server, takes it back. An emptied collection FLINCH created is removed; others are never touched.',
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
  inflow: {
    term: 'Coming in, likely unwatched',
    body: 'Monitored shows nobody started whose taste reads cold, shows abandoned partway over 180 days ago, open Seerr requests that read cold, and open requests for titles that already stream on a service you have. Advice unless you approve a show under Settings → Rules → Inflow actions; GiB/season is the mean of the seasons on disk.',
  },
  inflow_actions: {
    term: 'Inflow actions',
    body: 'Off by default. While a disk is over its target, FLINCH unmonitors the future seasons of the suggested shows you ticked (Sonarr stops adding new seasons and searching seasons with no file; seasons on disk keep theirs) and switches off automatic add on the import lists you ticked, switching them back on once no disk is over target. A show no longer suggested is left alone; a list that was already off is never switched on. Each write is read back; a dry run only prints it.',
  },
  ignore_viewers: {
    term: 'Ignored viewers',
    body: 'Viewers whose plays count as no play: a guest, a kid’s profile, your own test plays. Matched by name within each source. Evidence health stays as read, so an item only they played reads as never played only where every source was read in full; a partial read still proves nothing. Plex’s own item state belongs to the token’s account and cannot be split by viewer, so it still counts.',
  },
  desired_ratio: {
    term: 'Desired ratio',
    body: 'A seed ratio softer than the goal (Settings → Torrents). An item whose torrent is below it is not kept, only spared: it goes only once nothing else on its disk fills the target. A rule that keeps, prefers or forces the item still decides; pins, partway protection and Leaving Soon still apply.',
  },
  themes: {
    term: 'Themes',
    body: 'The library grouped by what titles are about: EmbeddingGemma 2 vectors clustered once a day, each group named by its most common genres. Played = titles anyone played in the last 365 days. A theme of 5+ titles under 10% played is seldom played. Display and quality advice only: themes never change what the plan evicts.',
  },
  meaning_search: {
    term: 'Search by meaning',
    body: 'Finds titles by what they are about, not their name: your words and each title’s description (genres, cast, overview) are compared as EmbeddingGemma 2 vectors, closest first. Titles not embedded yet are left out. Search only: it never changes the plan.',
  },
  notifications: {
    term: 'Notifications',
    body: 'Posts to Discord, ntfy, Apprise or a JSON webhook: titles entering Leaving Soon (with a Keep link: the household’s no-login link when those are on, else the title here), deletions, problems seen three runs in a row, a daily digest, and the weekly household newsletter for channels subscribed to it. Each event goes to a channel once; past the hourly limit the rest wait for a later run. Secret URLs stay in flinch-arrd’s environment.',
  },
  quality_actions: {
    term: 'Quality actions',
    body: 'Off by default. When on, an item advised a downgrade moves to the compact quality profile and Radarr or Sonarr is asked to search, so a smaller copy replaces the file and the title stays. Only with a release on the indexers at least 30% smaller, within a daily cap, never for a pinned item, one someone is partway through or one being evicted; a show moves only when every season on disk is advised. Each move is recorded with whether a smaller file landed within 14 days.',
  },
  upgrade_churn: {
    term: 'Upgrade churn',
    body: 'An item Radarr or Sonarr grabbed more often than the limit in 30 days (a season pack counts once): a profile whose cutoff no release meets, or scores that keep out-bidding each other. Flagged by default; Settings can unmonitor it or turn upgrades off on its profile.',
  },
  upgrade_search: {
    term: 'Upgrade searches',
    body: 'Off by default. When on, the items Radarr or Sonarr report below their profile cutoff are searched likeliest watched first, a few a day, and only where the disk forecast has room for the largest release Prowlarr lists. Never a pinned item, one someone is partway through, one hard to get back, one in the plan or one grabbed again and again. Each search is recorded with whether the item reached its cutoff within 14 days; an item is not searched again for 30 days.',
  },
  streaming: {
    term: 'Streaming',
    body: 'Off by default. FLINCH asks TMDB which services stream each title in your region (data by JustWatch), 40 titles a run, each answer kept a week. A title on a service you subscribe to keeps only a quarter of its re-download cost above the floor, never less, and its reason says where it streams. Only subscription streaming counts, not rent or buy; a title not looked up yet, or whose lookup failed, gets no discount.',
  },
  watch_sources: {
    term: 'Watch sources',
    body: 'Tracearr and Trakt play logs, read every cycle and joined to the library by TMDB, TVDB or IMDb id, never by title. A play there protects an item like a Tautulli stream. Tracearr is read for every user, every page; when that read is complete, an item that arrived after its record began, at least 30 days ago, and that nobody played counts as never played. Trakt is one account per source and proves plays only. Tokens stay in environment variables; a source that fails holds never-played reclaim off.',
  },
  seeding: {
    term: 'Seeding',
    body: 'Torrents still holding an item, read from qBittorrent or Transmission (Settings → Torrents). An item stays while one of its torrents has neither reached its client’s share limit nor FLINCH’s minimum ratio or days. Its size counts only if deleting frees it: a file hardlinked to a torrent that stays frees nothing, so the item stays too, unless the native executor removes the torrent with it. A client that cannot be read, or links that cannot be checked, keep the item.',
  },
  rules: {
    term: 'Rules',
    body: 'Hard constraints you set in Settings → Rules, never a change to regret. Keep (or keep until N days after an item was added, last played or requested) takes an item out of the plan; prefer evict and must evict make it go first, or go whenever its disk needs space. Rolling retention keeps the newest N seasons of a continuing show, or its first season, and lets the seasons around them compete, which a plain keep would block. Pins, partway protection, missing evidence, the grace period and Leaving Soon still apply. Keep beats evict. A fact FLINCH could not read keeps, never evicts.',
  },
  trash_sync: {
    term: 'Quality profiles (TRaSH)',
    body: 'Off by default. FLINCH reads the TRaSH-Guides custom formats, quality profiles and quality sizes at one pinned commit and compares them with Radarr and Sonarr, as Recyclarr does. The Quality profiles tab lists every change; nothing is written until you apply the ones you select (or switch on automatic apply), and a dry run only prints them. FLINCH deletes only custom formats it created and no profile uses, unless you allow deleting your own; it never deletes a profile. Its presets keep a compact profile for downgrades, stop endless upgrades, skip the Remux-1080p stop-over and tie-break toward smaller files.',
  },
  format_score: {
    term: 'Custom format score',
    body: 'What a release earns in a quality profile for each custom format it matches; Radarr and Sonarr grab and upgrade toward the highest total. The guide sets one per format; an override in Settings wins. Upgrades stop at the profile’s “upgrade until score”, and an upgrade must gain at least its minimum upgrade score.',
  },
  executor: {
    term: 'Executor',
    body: 'Who deletes (Settings → Executor). Maintainerr, the default: FLINCH hands picked items to its collections and Maintainerr deletes. Native: FLINCH deletes through Radarr and Sonarr itself. Finished items go at once; items nobody finished wait in a Plex Leaving Soon collection for the window and go only if still picked and unplayed. Plex is read again right before every delete, every write is read back, and a dry run only prints.',
  },
  restore: {
    term: 'Restore',
    body: 'Undo of a native delete within 30 days: on its next run the daemon monitors the movie or season again and asks Radarr or Sonarr to search (a removed movie is added back). The download is new, so the planner’s grace period keeps it for a while; set the keep tag if it must stay.',
  },
  dupes: {
    term: 'Duplicates',
    body: 'Off by default (Settings → Duplicates). Movies held in several copies: Plex versions of one item, the same film in two libraries, or in two Radarr instances; and large folders under a Radarr or Sonarr root that no item owns, listed only. FLINCH recommends the copy to keep: the one Radarr tracks, then the one the household played, then the highest picture (or HD, when that is your preference or quality advice says downgrade). Nothing is removed until you choose a copy, confirm it, and Remove is on; a pinned item or one someone is partway through keeps every copy. A confirmed choice also lets Maintainerr take the item through the copy you kept.',
  },
  household: {
    term: 'Household requests',
    body: 'Off by default (Settings → Household). Links in notifications, the newsletter and the Plex Leaving Soon summary let anyone keep a title with one click, no login: it is pinned at once for 60 days and leaves the shelf on the next run. Requesters can also ask to remove their own titles; nothing goes until you approve, and then it goes first when its disk needs space, never past a pin, someone partway through, missing evidence or the Leaving Soon window. A keep beats a removal. Each link is signed (FLINCH_WEB_LINK_SECRET), does one thing for one title and expires; with requester messages on, whoever requested a title is named, @mentioned on Discord and told on their own ntfy topic, Apprise URL or email, unless names are hidden.',
  },
  archive: {
    term: 'Archive tier',
    body: 'Off by default (Settings → Archive). Instead of deleting, FLINCH can move a movie, or a whole series (Sonarr moves series, never single seasons), to an archive root folder on another disk, where it stays playable. The archive disk’s own forecast says how much it can take while staying under its target; the plan fills that room before it deletes anything, with the items whose deletion would hurt most, and deletes only what still does not fit. Pinned items, anything someone is partway through, items already handed over and items under a rule never move. A move waits for the grace runs like a deletion, then FLINCH asks Radarr or Sonarr to move the files, reads the new path back and credits the freed space until the disk shows it. Add each archive root as a root folder in its app and as a folder of the same media-server library. Dry run prints the moves.',
  },
};

/** Card layout: heading, then the glossary keys in reading order. */
export const GLOSSARY_SECTIONS = [
  ['Prediction', ['p_watch', 'regret', 'reacquisition', 'eviction_safety', 'advice', 'quality_actions', 'upgrade_search', 'upgrade_churn', 'trash_sync', 'format_score', 'model', 'themes', 'meaning_search', 'user_weights']],
  ['Freeing space', ['projection', 'plan', 'eligible', 'pinned', 'rules', 'grace_period', 'grace_runs', 'never_played', 'dry_run', 'seeding', 'desired_ratio', 'streaming', 'inflow', 'inflow_actions', 'archive']],
  ['Disks', ['pending', 'held', 'untracked', 'not_governed']],
  ['Evidence', ['evidence', 'evidence_complete', 'watch_sources', 'ignore_viewers']],
  ['Deleting', ['executor', 'maintainerr', 'leaving_soon', 'restore', 'dupes', 'unresolved', 'operator_keeps', 'released_gone', 'outside_deletions', 'notifications', 'household']],
];
