//! Household self-service: "keep this" and "I'm done, remove it", from a
//! link in a notification, the newsletter or the Plex shelf's summary, with
//! no login ([`link`]).
//!
//! A pure state machine over `requests.json`. Only the web writes the file
//! (a click, the admin's decision); the daemon reads it each cycle:
//!
//! - **Keep** acts at once, because keeping is always safe: until it expires
//!   (`household.keep_days`, 60 by default) the item is pinned
//!   ([`Pin::Requested`]), protected like a favorite and taken off the
//!   Leaving Soon shelf.
//! - **Remove** waits in the admin's queue. Approved, it becomes a
//!   `must_evict` rule for that one card ([`RequestBook::rules`]), which the
//!   planner honours only as rules are: never past a pin, a partway viewer,
//!   an evidence gate or the Leaving Soon window, and only when its disk
//!   needs space.
//! - **Keep beats remove.** An active keep silences an approved removal, and
//!   the pin wins inside the rules engine anyway.

pub mod link;

use crate::plan::{Exclusion, MediaCandidate, Pin};
use crate::rules::{Effect, Rule, Scope};
use link::{Action, Link};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// The request book, on the state volume. Written only by the web.
pub const REQUESTS_FILE: &str = "requests.json";
const DAY: u64 = 86_400;
/// Decided and expired requests stay listed this long, then go.
const RESOLVED_TTL_SECS: u64 = 90 * DAY;
/// Most requests kept at once; past it new ones are refused, never old
/// ones dropped unseen.
pub const MAX_REQUESTS: usize = 2_000;

/// `settings.json` `household`: the self-service links. Off by default; on,
/// links also need `FLINCH_WEB_LINK_SECRET` and `notify.ui_url`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HouseholdConfig {
    pub enabled: bool,
    /// How long a requested keep pins its item.
    pub keep_days: u32,
    /// How long a link without a leave date stays valid.
    pub link_days: u32,
    /// Offer "remove it" links (to the requester's own titles) at all.
    pub allow_remove: bool,
}

impl Default for HouseholdConfig {
    fn default() -> Self {
        Self { enabled: false, keep_days: 60, link_days: 14, allow_remove: true }
    }
}

/// A [`HouseholdConfig`] outside its bounds; the message names the field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct InvalidHousehold(pub &'static str);

impl HouseholdConfig {
    pub fn validate(&self) -> Result<(), InvalidHousehold> {
        if !(1..=365).contains(&self.keep_days) {
            return Err(InvalidHousehold("a requested keep (household.keep_days) lasts 1 to 365 days"));
        }
        if !(1..=link::MAX_LINK_DAYS as u32).contains(&self.link_days) {
            return Err(InvalidHousehold("a link (household.link_days) stays valid 1 to 90 days"));
        }
        Ok(())
    }
}

/// Where a request stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// A keep, pinning its item until `until`.
    Active,
    /// A keep past `until`.
    Expired,
    /// A removal waiting for the admin.
    Pending,
    /// A removal the admin approved: a `must_evict` rule.
    Approved,
    Denied,
    /// Withdrawn by the admin (a keep ended early, a removal taken back).
    Cancelled,
}

/// One household request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    /// The id of the token that made it ([`link::token_id`]).
    pub id: String,
    pub card_id: String,
    pub title: String,
    pub kind: Action,
    pub by: String,
    pub at: u64,
    /// When a keep ends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<u64>,
    pub status: Status,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_at: Option<u64>,
}

impl Request {
    fn is_keep_at(&self, now: u64) -> bool {
        self.kind == Action::Keep && self.status == Status::Active && self.until.is_some_and(|until| now < until)
    }

    fn resolved(&self) -> bool {
        matches!(self.status, Status::Expired | Status::Denied | Status::Cancelled)
    }
}

/// What a click did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recorded {
    /// A new request.
    New(Request),
    /// The token was spent before, or the card already had the same request:
    /// nothing changed but a keep's end moving later.
    Existing(Request),
}

impl Recorded {
    pub fn request(&self) -> &Request {
        match self {
            Self::New(request) | Self::Existing(request) => request,
        }
    }
}

/// Why a request was not taken. Words for the household's page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Refused {
    #[error("removal requests are switched off")]
    RemoveOff,
    #[error("too many open requests; ask the admin")]
    Full,
    #[error("no such request")]
    Unknown,
    #[error("that request cannot be changed that way")]
    Settled,
}

/// The admin's decision on one request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Approve,
    Deny,
    Cancel,
}

/// Every request, oldest first.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RequestBook {
    pub requests: Vec<Request>,
}

impl RequestBook {
    /// A missing or unreadable file is an empty book: no pin, no removal, so
    /// a damaged file can only keep less, never take more.
    pub fn read(dir: &Path) -> Self {
        std::fs::read(dir.join(REQUESTS_FILE)).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default()
    }

    pub fn write(&self, dir: &Path) -> std::io::Result<()> {
        crate::persist::replace(&dir.join(REQUESTS_FILE), &serde_json::to_vec_pretty(self)?)
    }

    /// The request a verified token makes. `token_id` names it, so a second
    /// click returns the first. A keep for a card already kept moves the
    /// end later; a removal for a card already queued or approved is that one.
    pub fn record(&mut self, link: &Link, token_id: &str, title: &str, config: &HouseholdConfig, now: u64) -> Result<Recorded, Refused> {
        if let Some(spent) = self.requests.iter().find(|request| request.id == token_id) {
            return Ok(Recorded::Existing(spent.clone()));
        }
        match link.action {
            Action::Keep => {
                let until = now + u64::from(config.keep_days) * DAY;
                if let Some(kept) = self.requests.iter_mut().find(|request| request.card_id == link.card_id && request.is_keep_at(now)) {
                    kept.until = kept.until.max(Some(until));
                    return Ok(Recorded::Existing(kept.clone()));
                }
            }
            Action::Remove => {
                if !config.allow_remove {
                    return Err(Refused::RemoveOff);
                }
                let open = |request: &&Request| {
                    request.card_id == link.card_id
                        && request.kind == Action::Remove
                        && matches!(request.status, Status::Pending | Status::Approved)
                };
                if let Some(queued) = self.requests.iter().find(open) {
                    return Ok(Recorded::Existing(queued.clone()));
                }
            }
        }
        self.prune(now);
        if self.requests.len() >= MAX_REQUESTS {
            return Err(Refused::Full);
        }
        let (status, until) = match link.action {
            Action::Keep => (Status::Active, Some(now + u64::from(config.keep_days) * DAY)),
            Action::Remove => (Status::Pending, None),
        };
        let request = Request {
            id: token_id.to_string(),
            card_id: link.card_id.clone(),
            title: title.to_string(),
            kind: link.action,
            by: link.by.clone(),
            at: now,
            until,
            status,
            decided_at: None,
        };
        self.requests.push(request.clone());
        Ok(Recorded::New(request))
    }

    /// The admin decides: approve or deny a pending removal, cancel an
    /// active keep or an approved removal.
    pub fn decide(&mut self, id: &str, decision: Decision, now: u64) -> Result<&Request, Refused> {
        let request = self.requests.iter_mut().find(|request| request.id == id).ok_or(Refused::Unknown)?;
        request.status = match (request.kind, request.status, decision) {
            (Action::Remove, Status::Pending, Decision::Approve) => Status::Approved,
            (Action::Remove, Status::Pending, Decision::Deny) => Status::Denied,
            (Action::Remove, Status::Approved, Decision::Cancel) | (Action::Keep, Status::Active, Decision::Cancel) => Status::Cancelled,
            _ => return Err(Refused::Settled),
        };
        request.decided_at = Some(now);
        Ok(request)
    }

    /// Keeps past their end become expired; resolved requests older than
    /// [`RESOLVED_TTL_SECS`] go.
    pub fn prune(&mut self, now: u64) {
        for request in &mut self.requests {
            if request.kind == Action::Keep && request.status == Status::Active && !request.is_keep_at(now) {
                request.status = Status::Expired;
                request.decided_at = request.until;
            }
        }
        self.requests.retain(|request| {
            let settled = request.decided_at.unwrap_or(request.at);
            !(request.resolved() && now.saturating_sub(settled) >= RESOLVED_TTL_SECS)
        });
    }

    /// Open removals whose card left the library are settled: nothing is left
    /// to remove. `present` is every card id in the library.
    pub fn forget_gone(&mut self, present: &HashSet<&str>, now: u64) {
        for request in &mut self.requests {
            if matches!(request.status, Status::Pending | Status::Approved) && !present.contains(request.card_id.as_str()) {
                request.status = Status::Cancelled;
                request.decided_at = Some(now);
            }
        }
    }

    /// Card id → when its requested keep ends, for every keep active at `now`.
    pub fn pins(&self, now: u64) -> HashMap<&str, u64> {
        let mut pins: HashMap<&str, u64> = HashMap::new();
        for request in self.requests.iter().filter(|request| request.is_keep_at(now)) {
            let until = request.until.unwrap_or(now);
            pins.entry(request.card_id.as_str()).and_modify(|end| *end = (*end).max(until)).or_insert(until);
        }
        pins
    }

    /// One `must_evict` rule per approved removal whose card nobody keeps.
    pub fn rules(&self, now: u64) -> Vec<Rule> {
        let kept = self.pins(now);
        let mut seen = HashSet::new();
        self.requests
            .iter()
            .filter(|request| request.kind == Action::Remove && request.status == Status::Approved)
            .filter(|request| !kept.contains_key(request.card_id.as_str()) && seen.insert(request.card_id.as_str()))
            .map(|request| Rule {
                name: format!("removal request by {} ({})", request.by, request.card_id),
                enabled: true,
                scope: Scope { ids: vec![request.card_id.clone()], ..Scope::default() },
                effect: Effect::MustEvict,
            })
            .collect()
    }

    /// The counts `status.json` `household` shows.
    pub fn status(&self, links: bool, now: u64) -> HouseholdStatus {
        let count = |status: Status| self.requests.iter().filter(|request| request.status == status).count();
        HouseholdStatus {
            links,
            active_keeps: self.requests.iter().filter(|request| request.is_keep_at(now)).count(),
            pending_removals: count(Status::Pending),
            approved_removals: count(Status::Approved),
            ..HouseholdStatus::default()
        }
    }
}

/// Pin every candidate a household member asked to keep. A favorite or keep
/// list stays the reason when there is one; any other exclusion gives way,
/// as a pin outranks it in the builder. The item is protected, so neither
/// Maintainerr nor the native executor may take it, and any force is gone:
/// keep beats remove.
pub fn pin(candidates: &mut [MediaCandidate], pins: &HashMap<&str, u64>) -> usize {
    let mut pinned = 0;
    for candidate in candidates.iter_mut() {
        let Some(&until) = pins.get(candidate.id.as_str()) else { continue };
        if !matches!(candidate.exclusion, Some(Exclusion::Pinned(_))) {
            candidate.exclusion = Some(Exclusion::Pinned(Pin::Requested { until }));
        }
        candidate.protect = true;
        candidate.force = None;
        pinned += 1;
    }
    pinned
}

/// Titles a shelf summary lists; the rest are counted.
const SUMMARY_TITLES: usize = 25;

/// Whether this install makes no-login links: the feature is on, the secret
/// is set in the environment ([`link::SECRET_ENV`]) and FLINCH's address is
/// known. While it is, the household step writes the Leaving Soon shelf's
/// summary, and nothing else does.
pub fn links_on(config: &HouseholdConfig, ui_url: &str) -> bool {
    config.enabled && link::url(ui_url, "").is_some() && link::LinkSecret::from_env().is_some()
}

/// The Leaving Soon shelf's summary: `heading` (the shelf's dates line) when
/// there is one, then each title, when it leaves and its keep link, soonest
/// first. `items` are (title, leaves at, keep link).
pub fn shelf_summary(heading: Option<&str>, items: &[(String, u64, String)]) -> String {
    let mut sorted: Vec<&(String, u64, String)> = items.iter().collect();
    sorted.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
    let mut lines: Vec<String> = heading.map(str::to_string).into_iter().collect();
    lines.push("Play a title to keep it, or open its link:".to_string());
    lines.extend(
        sorted
            .iter()
            .take(SUMMARY_TITLES)
            .map(|(title, leaves, url)| format!("• {title} (leaves after {}): {url}", crate::notify::utc_date(leaves / DAY))),
    );
    if sorted.len() > SUMMARY_TITLES {
        lines.push(format!("…and {} more", sorted.len() - SUMMARY_TITLES));
    }
    lines.join("\n")
}

/// `status.json` `household`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HouseholdStatus {
    /// Links can be made: the feature is on, the secret is set and FLINCH's
    /// address is known.
    pub links: bool,
    pub active_keeps: usize,
    pub pending_removals: usize,
    pub approved_removals: usize,
    /// People the household notifications reach ([`crate::notify::recipients`]).
    #[serde(default)]
    pub recipients: usize,
    /// Why something did not happen this cycle (links off for a missing
    /// secret, a shelf summary Plex refused), in words.
    #[serde(default)]
    pub problems: Vec<String>,
}

#[cfg(test)]
mod tests;
