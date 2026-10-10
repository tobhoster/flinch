//! The weekly household newsletter: what is leaving soon, with each title's
//! TMDB poster and a keep link, what left during the week, and, in a
//! person's own copy, their requests still on disk with a link to say they
//! are done with one.
//!
//! Posters are TMDB CDN addresses only ([`super::recipients::tmdb_poster`]),
//! never a Plex or *arr URL: those carry a token or reach a private host.

use super::render::{date, gib, Card, Level, Message};
use super::utc_date;
use serde::{Deserialize, Serialize};

const DAY: u64 = 86_400;
/// Titles listed per section; the rest are counted.
const MAX_ITEMS: usize = 15;
/// Discord shows at most ten embeds, one of them the newsletter itself.
pub(super) const MAX_CARDS: usize = 9;

/// One week's newsletter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Newsletter {
    /// Monday-based weeks since the epoch ([`week_of`]).
    pub week: u64,
    /// On the Leaving Soon shelf, soonest first.
    pub leaving: Vec<NewsItem>,
    /// A person's own requests still on disk (their copy only).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub yours: Vec<NewsItem>,
    /// Titles, and their bytes, that left the library in the last 7 days.
    pub left_items: usize,
    pub left_bytes: u64,
}

/// One title in the newsletter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewsItem {
    pub id: String,
    pub title: String,
    pub bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leaves_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub poster: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requesters: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remove_url: Option<String>,
}

/// Monday-based weeks since the epoch (1970-01-01 was a Thursday).
pub fn week_of(unix: u64) -> u64 {
    (unix / DAY + 3) / 7
}

/// Whether this week's newsletter is due at `now`: from `weekday` (0
/// Monday) at `hour` UTC to the week's end, so a daemon that was down that
/// hour still sends it later in the week.
pub fn due(now: u64, weekday: u8, hour: u8) -> bool {
    let into_week = (now / DAY + 3) % 7;
    let hour_now = (now % DAY) / 3_600;
    into_week > u64::from(weekday) || (into_week == u64::from(weekday) && hour_now >= u64::from(hour))
}

/// "requested by A and B", or nothing when names are hidden or unknown.
pub(super) fn requested_by(requesters: &[String], hide: bool) -> String {
    match (hide, requesters) {
        (true, _) | (_, []) => String::new(),
        (false, [one]) => format!(" · requested by {one}"),
        (false, [rest @ .., last]) => format!(" · requested by {} and {last}", rest.join(", ")),
    }
}

fn listed(lines: &mut Vec<String>, items: &[NewsItem], line: impl Fn(&NewsItem) -> String) {
    lines.extend(items.iter().take(MAX_ITEMS).map(line));
    if items.len() > MAX_ITEMS {
        lines.push(format!("…and {} more", items.len() - MAX_ITEMS));
    }
}

/// The newsletter as one message: lines for every channel, a card with
/// the poster per Leaving Soon title for the channels that show images.
pub(super) fn message(newsletter: &Newsletter, hide_requester: bool) -> Message {
    let monday = (newsletter.week * 7).saturating_sub(3);
    let mut lines = vec![format!("Week of {}.", utc_date(monday))];
    if newsletter.leaving.is_empty() {
        lines.push("Nothing is leaving soon.".to_string());
    } else {
        lines.push("**Leaving soon**: play one to keep it, or use its Keep link.".to_string());
        listed(&mut lines, &newsletter.leaving, |item| {
            let when = item.leaves_at.map_or_else(|| "soon".to_string(), |at| format!("after {}", date(at)));
            let keep = item.keep_url.as_ref().map_or_else(String::new, |url| format!(" · [Keep]({url})"));
            format!("- **{}** · {} · leaves {when}{}{keep}", item.title, gib(item.bytes), requested_by(&item.requesters, hide_requester))
        });
    }
    lines.push(format!("Left the library this week: {} title(s), {}.", newsletter.left_items, gib(newsletter.left_bytes)));
    if !newsletter.yours.is_empty() {
        lines.push("**Your requests still here**: done with one? Ask for it to go.".to_string());
        listed(&mut lines, &newsletter.yours, |item| {
            let remove = item.remove_url.as_ref().map_or_else(String::new, |url| format!(" · [Remove]({url})"));
            format!("- {} · {}{remove}", item.title, gib(item.bytes))
        });
    }
    let cards = newsletter
        .leaving
        .iter()
        .take(MAX_CARDS)
        .map(|item| Card {
            title: item.title.clone(),
            url: item.keep_url.clone(),
            text: item.leaves_at.map_or_else(|| "Leaving soon".to_string(), |at| format!("Leaves after {}", date(at))),
            image: item.poster.clone(),
        })
        .collect();
    let links = newsletter.leaving.iter().filter_map(|item| Some((format!("Keep {}", item.title), item.keep_url.clone()?))).collect();
    let title = format!("This week: {} title(s) leaving soon", newsletter.leaving.len());
    Message { title, lines, links, level: Level::Info, cards, mentions: Vec::new() }
}
