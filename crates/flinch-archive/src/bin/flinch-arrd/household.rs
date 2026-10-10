//! The household for one cycle: requested keeps pin their items, approved
//! removals join the operator's rules as `must_evict`, no-login links are
//! signed for notifications and the Plex shelf, and requesters get their own
//! copy of what concerns them ([`flinch_archive::requests`],
//! [`flinch_archive::notify::recipients`]).
//!
//! The request book is the web's to write; the daemon only reads it. With
//! the feature off nothing here runs, so an old book pins nothing.

use super::state_dir;
use flinch_archive::daemon::RuntimeSettings;
use flinch_archive::executor::{Executor, NativeState, ShelfServer};
use flinch_archive::ids::PlexIds;
use flinch_archive::maintainerr::Route;
use flinch_archive::notify::newsletter::{self, NewsItem, Newsletter};
use flinch_archive::notify::recipients::{self, Recipient};
use flinch_archive::notify::{Event, Personal};
use flinch_archive::plan::candidates::Library;
use flinch_archive::plan::MediaCandidate;
use flinch_archive::plex::collections::PlexCollections;
use flinch_archive::requests::link::{Action, LinkSecret, Signer, SECRET_ENV};
use flinch_archive::requests::{self, HouseholdStatus, RequestBook};
use flinch_archive::rules::Rule;
use flinch_archive::signals::seerr::Contact;
use flinch_archive::ItemSnapshot;
use std::collections::{BTreeMap, HashMap};

/// Who a link in a shared message or on the shelf is addressed to.
const SHARED: &str = "household";
/// A person's own titles their newsletter offers to remove.
const YOURS: usize = 10;
const WEEK_SECS: u64 = 7 * 86_400;
/// One Plex shelf: its section, and per item (title, leave date, keep link).
type Shelf = (Option<u32>, Vec<(String, u64, String)>);

/// The cycle's household, its keeps pinned on `candidates`, and the rules in
/// force: the operator's, then one `must_evict` per approved removal. Runs
/// before the rules, so a keep stays the reason and no rule can force it.
pub(super) fn prepare(
    settings: &RuntimeSettings,
    library: &Library<'_>,
    contacts: &[Contact],
    candidates: &mut [MediaCandidate],
    now: u64,
) -> (Option<Household>, Vec<Rule>) {
    let household = Household::gather(settings, library, contacts, now);
    let mut rules = settings.rules.clone();
    if let Some(household) = &household {
        household.pin(candidates);
        rules.extend(household.book.rules(now));
    }
    (household, rules)
}

/// After the executor ran: the shelf summaries, when there is a household.
pub(super) async fn shelves(
    household: Option<&mut Household>,
    http: &reqwest::Client,
    settings: &RuntimeSettings,
    plex_ids: &HashMap<String, PlexIds>,
    dry_run: bool,
) {
    if let Some(household) = household {
        household.write_shelves(http, settings, plex_ids, dry_run).await;
    }
}

pub(super) struct Household {
    book: RequestBook,
    secret: Option<LinkSecret>,
    /// Card id → Seerr requesters, while requester messages are on.
    requesters: HashMap<String, Vec<String>>,
    pub(super) people: Vec<Recipient>,
    status: HouseholdStatus,
    now: u64,
}

impl Household {
    /// `None` while every household feature is off.
    fn gather(settings: &RuntimeSettings, library: &Library<'_>, contacts: &[Contact], now: u64) -> Option<Self> {
        let (links, notify) = (&settings.household, &settings.notify.household);
        if !links.enabled && !notify.enabled && !notify.newsletter {
            return None;
        }
        let book = if links.enabled { RequestBook::read(&state_dir()) } else { RequestBook::default() };
        let secret = requests::links_on(links, &settings.notify.ui_url).then(LinkSecret::from_env).flatten();
        let mut status = book.status(secret.is_some(), now);
        if links.enabled && secret.is_none() {
            status
                .problems
                .push(format!("no keep or remove links: {SECRET_ENV} needs at least 32 characters and notify.ui_url FLINCH's address"));
        }
        let (requesters, people) = if notify.enabled || notify.newsletter {
            (recipients::requesters_by_card(library), recipients::resolve(notify, contacts))
        } else {
            (HashMap::new(), Vec::new())
        };
        status.recipients = people.len();
        Some(Self { book, secret, requesters, people, status, now })
    }

    /// Pin every item someone asked to keep.
    fn pin(&self, candidates: &mut [MediaCandidate]) {
        let pinned = requests::pin(candidates, &self.book.pins(self.now));
        if pinned > 0 {
            println!("[flinch-arrd] household: {pinned} item(s) kept on request");
        }
    }

    pub(super) fn status(&self) -> HouseholdStatus {
        self.status.clone()
    }

    fn link(&self, settings: &RuntimeSettings, card: &str, action: Action, leaves_at: Option<u64>, by: &str) -> Option<String> {
        let secret = self.secret.as_ref()?;
        let signer = Signer { secret, ui_url: &settings.notify.ui_url, days: settings.household.link_days, now: self.now };
        signer.link(card, action, leaves_at, by)
    }

    fn requesters_of(&self, card: &str) -> Vec<String> {
        self.requesters.get(card).cloned().unwrap_or_default()
    }

    /// A Leaving Soon event as the household sees it: who asked for it, its
    /// TMDB poster and the shared keep link.
    pub(super) fn leaving(&self, settings: &RuntimeSettings, item: &ItemSnapshot, handed_at: u64, title: String) -> Event {
        Event::LeavingSoon {
            id: item.id.clone(),
            title,
            bytes: item.size_bytes,
            handed_at,
            leaves_at: item.leaves_at,
            requesters: self.requesters_of(&item.id),
            poster: item.poster_url.as_deref().and_then(recipients::tmdb_poster),
            keep_url: self.link(settings, &item.id, Action::Keep, item.leaves_at, SHARED),
        }
    }

    fn news_item(&self, settings: &RuntimeSettings, item: &ItemSnapshot, by: &str) -> NewsItem {
        NewsItem {
            id: item.id.clone(),
            title: super::notices::display_title(item),
            bytes: item.size_bytes,
            leaves_at: item.leaves_at,
            poster: item.poster_url.as_deref().and_then(recipients::tmdb_poster),
            requesters: self.requesters_of(&item.id),
            keep_url: self.link(settings, &item.id, Action::Keep, item.leaves_at, by),
            remove_url: None,
        }
    }

    /// This week's newsletter for `by` (the shared one for [`SHARED`]),
    /// when it is due; `left` is (titles, bytes) gone in the last 7 days.
    pub(super) fn newsletter(
        &self,
        settings: &RuntimeSettings,
        items: &[ItemSnapshot],
        left: (usize, u64),
        by: &str,
    ) -> Option<Newsletter> {
        let config = &settings.notify.household;
        if !config.newsletter || !newsletter::due(self.now, config.newsletter_weekday, config.newsletter_hour_utc) {
            return None;
        }
        let mut leaving: Vec<NewsItem> =
            items.iter().filter(|item| item.route == Some(Route::LeavingSoon)).map(|item| self.news_item(settings, item, by)).collect();
        leaving.sort_by(|a, b| a.leaves_at.cmp(&b.leaves_at).then_with(|| a.title.cmp(&b.title)));
        let mut yours: Vec<NewsItem> = Vec::new();
        if by != SHARED && settings.household.allow_remove {
            let theirs = items.iter().filter(|item| {
                item.route != Some(Route::LeavingSoon) && self.requesters_of(&item.id).iter().any(|name| name.eq_ignore_ascii_case(by))
            });
            yours = theirs
                .filter_map(|item| {
                    let remove_url = self.link(settings, &item.id, Action::Remove, None, by)?;
                    Some(NewsItem { remove_url: Some(remove_url), keep_url: None, ..self.news_item(settings, item, by) })
                })
                .collect();
            yours.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.title.cmp(&b.title)));
            yours.truncate(YOURS);
        }
        Some(Newsletter { week: newsletter::week_of(self.now), leaving, yours, left_items: left.0, left_bytes: left.1 })
    }

    /// Each recipient's own events: the Leaving Soon titles they requested,
    /// with links signed for them, and their newsletter.
    pub(super) fn personal(
        &self,
        settings: &RuntimeSettings,
        items: &[ItemSnapshot],
        events: &[Event],
        left: (usize, u64),
    ) -> Vec<Personal> {
        if !settings.notify.household.enabled && !settings.notify.household.newsletter {
            return Vec::new();
        }
        self.people
            .iter()
            .filter(|person| person.has_address())
            .map(|person| {
                let mut theirs: Vec<Event> = Vec::new();
                if settings.notify.household.enabled {
                    for event in events {
                        let Event::LeavingSoon { requesters, .. } = event else { continue };
                        if !requesters.iter().any(|name| name.eq_ignore_ascii_case(&person.name)) {
                            continue;
                        }
                        let mut own = event.clone();
                        if let Event::LeavingSoon { id, keep_url, leaves_at, .. } = &mut own {
                            *keep_url = self.link(settings, id, Action::Keep, *leaves_at, &person.name);
                        }
                        theirs.push(own);
                    }
                }
                if person.newsletter {
                    theirs.extend(self.newsletter(settings, items, left, &person.name).map(Event::Newsletter));
                }
                Personal { recipient: person.clone(), events: theirs }
            })
            .filter(|personal| !personal.events.is_empty())
            .collect()
    }

    /// Write each native Leaving Soon shelf's summary: its dates, then every
    /// title with its keep link. Only while links are on (otherwise the
    /// shelf keeps its dates line alone) and only on FLINCH's own shelf:
    /// Maintainerr's collection is Maintainerr's to describe.
    async fn write_shelves(
        &mut self,
        http: &reqwest::Client,
        settings: &RuntimeSettings,
        plex_ids: &HashMap<String, PlexIds>,
        dry_run: bool,
    ) {
        if settings.executor != Executor::Native || self.secret.is_none() {
            return;
        }
        let Some((url, token)) = super::evidence::plex_connection(settings) else { return };
        let state = NativeState::read(&state_dir());
        // Collection ratingKey → (section, [(title, leaves, link)]).
        let mut shelves: BTreeMap<&str, Shelf> = BTreeMap::new();
        for (id, entry) in state.leaving.iter().filter(|(_, entry)| entry.server == ShelfServer::Plex) {
            let shelf = shelves.entry(entry.collection.as_str()).or_default();
            shelf.0 = shelf.0.or_else(|| plex_ids.get(id).and_then(|ids| ids.section_id));
            if let Some(link) = self.link(settings, id, Action::Keep, Some(entry.until), SHARED) {
                shelf.1.push((entry.title.clone(), entry.until, link));
            }
        }
        let plex = PlexCollections::new(http, &url, &token, dry_run);
        for (collection, (section, items)) in shelves {
            let Some(section) = section else { continue };
            let untils: Vec<u64> = items.iter().map(|(_, until, _)| *until).collect();
            let heading = flinch_archive::plex::shelf::dates_line(&untils);
            let summary = requests::shelf_summary(heading.as_deref(), &items);
            if let Err(error) = plex.edit_summary(section, collection, &summary).await {
                self.status.problems.push(format!("Leaving Soon summary not written: {error}"));
            }
        }
        for problem in &self.status.problems {
            eprintln!("[flinch-arrd] household: {problem}");
        }
    }

    /// Titles and bytes that left the library in the last week.
    pub(super) fn left_this_week(ledger: &flinch_archive::capacity::EvictionLedger, now: u64) -> (usize, u64) {
        let gone: Vec<u64> = ledger
            .entries
            .values()
            .filter(|eviction| eviction.gone_at.is_some_and(|gone| now.saturating_sub(gone) < WEEK_SECS))
            .map(|eviction| eviction.bytes)
            .collect();
        (gone.len(), gone.iter().sum())
    }
}
