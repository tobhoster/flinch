//! Who a notification about a title is for: the people who requested it in
//! Seerr, with the addresses Seerr and the operator's overrides give them.
//!
//! Seerr's `GET /api/v1/user` (<https://api-docs.overseerr.dev/>, schema
//! `User`) gives each user's display name (the name requests carry), email
//! and media-server names; [`crate::signals::seerr::parse_contacts`] reads
//! them. An override names a user by any of those, and adds what Seerr does
//! not know: a Discord id, an ntfy topic, an Apprise URL, another email.

use super::household::{is_email, HouseholdNotify, RecipientOverride};
use crate::plan::candidates::{cards_of, Library};
use crate::signals::seerr::Contact;
use std::collections::{BTreeMap, HashMap, HashSet};

/// One person the household notifications reach.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Recipient {
    /// The Seerr display name, as requests name them.
    pub name: String,
    pub discord_id: Option<String>,
    pub ntfy_topic: Option<String>,
    pub apprise_env: Option<String>,
    pub email: Option<String>,
    pub newsletter: bool,
}

impl Recipient {
    /// Has somewhere of their own to be told (a mention is not one).
    pub fn has_address(&self) -> bool {
        self.ntfy_topic.is_some() || self.apprise_env.is_some() || self.email.is_some()
    }
}

fn some(text: &str) -> Option<String> {
    (!text.is_empty()).then(|| text.to_string())
}

fn matches(contact: &Contact, user: &str) -> bool {
    contact.name.eq_ignore_ascii_case(user) || contact.aliases.iter().any(|alias| alias.eq_ignore_ascii_case(user))
}

/// Every person with an address or a Discord id, by Seerr name. A Seerr user
/// gets their Seerr email only when `email_seerr_users` is on; an override
/// adds to (or, muted, removes) whoever it names, and an override naming
/// nobody in Seerr stands for a person of that name.
pub fn resolve(config: &HouseholdNotify, contacts: &[Contact]) -> Vec<Recipient> {
    let mut people: BTreeMap<String, Recipient> = BTreeMap::new();
    let mut muted: HashSet<String> = HashSet::new();
    for contact in contacts {
        let email = contact.email.clone().filter(|email| config.email_seerr_users && is_email(email));
        people
            .insert(contact.name.to_lowercase(), Recipient { name: contact.name.clone(), email, newsletter: true, ..Recipient::default() });
    }
    for over in &config.recipients {
        let name = contacts.iter().find(|contact| matches(contact, &over.user)).map_or(over.user.as_str(), |contact| &contact.name);
        if over.muted {
            muted.insert(name.to_lowercase());
            continue;
        }
        let person = people.entry(name.to_lowercase()).or_insert_with(|| Recipient { name: name.to_string(), ..Recipient::default() });
        apply(person, over);
    }
    people
        .into_iter()
        .filter(|(key, person)| !muted.contains(key) && (person.has_address() || person.discord_id.is_some()))
        .map(|(_, person)| person)
        .collect()
}

fn apply(person: &mut Recipient, over: &RecipientOverride) {
    person.discord_id = some(&over.discord_id).or(person.discord_id.take());
    person.ntfy_topic = some(&over.ntfy_topic).or(person.ntfy_topic.take());
    person.apprise_env = some(&over.apprise_env).or(person.apprise_env.take());
    person.email = some(&over.email).or(person.email.take());
    person.newsletter = over.newsletter;
}

/// The recipient a requester's name points at.
pub fn find<'a>(recipients: &'a [Recipient], requester: &str) -> Option<&'a Recipient> {
    recipients.iter().find(|person| person.name.eq_ignore_ascii_case(requester))
}

/// Card id → who requested it (Seerr names, deduplicated, in request order).
/// Empty while Seerr's requests were not read this cycle.
pub fn requesters_by_card(library: &Library) -> HashMap<String, Vec<String>> {
    let mut by_card: HashMap<String, Vec<String>> = HashMap::new();
    if !library.signals.requests_read {
        return by_card;
    }
    let known: HashSet<&str> = library.cards.iter().map(|card| card.id.as_str()).collect();
    for request in &library.signals.requests {
        for card in cards_of(library, &known, &request.media, &request.seasons) {
            let names = by_card.entry(card.to_string()).or_default();
            if !names.iter().any(|name| name.eq_ignore_ascii_case(&request.requester)) {
                names.push(request.requester.clone());
            }
        }
    }
    by_card
}

/// TMDB's image CDN, and the size a message shows (342 px wide).
const TMDB_IMAGES: &str = "https://image.tmdb.org/t/p/";
const POSTER_SIZE: &str = "w342";

/// A poster a message may show: TMDB's own CDN only, resized. Anything
/// else is dropped, above all a Plex or *arr address: those carry the
/// server's token or reach a private host.
pub fn tmdb_poster(url: &str) -> Option<String> {
    let path = url.strip_prefix(TMDB_IMAGES)?;
    let (_, file) = path.split_once('/')?;
    let plain = !file.is_empty() && file.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte));
    plain.then(|| format!("{TMDB_IMAGES}{POSTER_SIZE}/{file}"))
}
