//! Events into each channel's request body. Pure: what goes on the wire is
//! decided here and tested here, the posting is in [`super::outbox`].
//!
//! Shapes, from each service's own documentation:
//! - Discord: execute webhook, `content`/`embeds`/`allowed_mentions`,
//!   <https://discord.com/developers/docs/resources/webhook#execute-webhook>;
//!   embed limits (title 256, description 4096 characters),
//!   <https://discord.com/developers/docs/resources/message#embed-object-embed-limits>;
//!   `allowed_mentions.users` (at most 100 ids) pings only the listed users,
//!   <https://discord.com/developers/docs/resources/message#allowed-mentions-object>;
//!   a card's poster is the embed's `thumbnail.url`.
//! - ntfy: publish as JSON to the server root with `topic`, `title`,
//!   `message`, `markdown`, `tags`, `priority`, `click` and up to three `view`
//!   actions; messages up to 4,096 bytes, <https://docs.ntfy.sh/publish/#publish-as-json>.
//! - Apprise API: `POST /notify/{KEY}` with `title`, `body`, `type`
//!   (info|success|warning|failure) and `format` (text|markdown|html),
//!   <https://github.com/caronc/apprise-api#api-details>.
//! - Webhook: FLINCH's own events, `{"source": "flinch", "events": [...]}`.

use super::recipients::{self, Recipient};
use super::{newsletter, utc_date, ChannelKind, Event, EventKind, NotifyConfig, Stage};
use serde_json::{json, Value};

/// One message, before a channel formats it.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Message {
    pub(super) title: String,
    /// Markdown lines.
    pub(super) lines: Vec<String>,
    /// (label, url) per item, for channels with buttons.
    pub(super) links: Vec<(String, String)>,
    pub(super) level: Level,
    /// One per title with a picture, for channels that show images.
    pub(super) cards: Vec<Card>,
    /// Discord user ids to mention.
    pub(super) mentions: Vec<String>,
}

/// A title as a card: a Discord embed with its poster as the thumbnail, an
/// image in Apprise's markdown.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Card {
    pub(super) title: String,
    pub(super) url: Option<String>,
    pub(super) text: String,
    pub(super) image: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Level {
    Info,
    Warning,
    Failure,
}

/// Most item lines in one message; the rest are counted, not listed.
const MAX_LINES: usize = 15;
const GIB: f64 = 1_073_741_824.0;
/// Discord embed limits, and ntfy's message limit in bytes.
const DISCORD_TITLE: usize = 256;
const DISCORD_DESCRIPTION: usize = 4_096;
/// The newsletter's own embed beside its cards: Discord allows 6,000
/// characters across one message's embeds.
const DISCORD_DESCRIPTION_WITH_CARDS: usize = 3_000;
const CARD_FIELD: usize = 120;
/// `allowed_mentions.users` takes at most 100 ids.
const DISCORD_MENTIONS: usize = 100;
const NTFY_MESSAGE: usize = 4_096;
/// ntfy shows at most three action buttons.
const NTFY_ACTIONS: usize = 3;

/// The ntfy server root and topic of a topic URL (`https://ntfy.sh/flinch`
/// → `https://ntfy.sh/`, `flinch`): JSON is published to the root. A query
/// (ntfy's `?auth=`) stays on the root.
pub(super) fn ntfy_topic(url: &str) -> Option<(String, String)> {
    let mut parsed = reqwest::Url::parse(url).ok()?;
    let mut segments: Vec<String> = parsed.path_segments()?.filter(|segment| !segment.is_empty()).map(str::to_string).collect();
    let topic = segments.pop()?;
    parsed.set_path(&format!("/{}", segments.iter().map(|segment| format!("{segment}/")).collect::<String>()));
    Some((parsed.to_string(), topic))
}

/// The Keep link of an item: FLINCH's UI opened on it.
pub(super) fn keep_url(ui_url: &str, id: &str) -> Option<String> {
    if ui_url.is_empty() {
        return None;
    }
    let mut url = reqwest::Url::parse(ui_url).ok()?;
    url.query_pairs_mut().clear().append_pair("item", id);
    Some(url.to_string())
}

pub(super) fn gib(bytes: u64) -> String {
    format!("{:.1} GiB", bytes as f64 / GIB)
}

pub(super) fn date(unix: u64) -> String {
    utc_date(unix / 86_400)
}

fn more(lines: &mut Vec<String>, total: usize) {
    if total > MAX_LINES {
        lines.truncate(MAX_LINES);
        lines.push(format!("…and {} more", total - MAX_LINES));
    }
}

/// The Discord ids of the people a set of requesters names, unless names
/// are hidden; each once.
fn mentions<'a>(requesters: impl Iterator<Item = &'a String>, config: &NotifyConfig, people: &[Recipient]) -> Vec<String> {
    if !config.household.enabled || config.household.hide_requester {
        return Vec::new();
    }
    let mut ids: Vec<String> = requesters.filter_map(|name| recipients::find(people, name)?.discord_id.clone()).collect();
    ids.sort();
    ids.dedup();
    ids
}

/// One message for every event of one kind. `people` are the household's
/// recipients, for mentions.
pub(super) fn message(kind: EventKind, events: &[&Event], config: &NotifyConfig, people: &[Recipient]) -> Message {
    let ui_url = config.ui_url.as_str();
    let hide = config.household.hide_requester;
    let mut lines = Vec::new();
    let mut links = Vec::new();
    let mut tagged = Vec::new();
    let (title, level) = match kind {
        EventKind::LeavingSoon => {
            for event in events {
                if let Event::LeavingSoon { id, title, bytes, leaves_at, requesters, keep_url: household, .. } = event {
                    let when = leaves_at.map_or_else(|| "on the next cleanup run".to_string(), |at| format!("after {}", date(at)));
                    let keep = household.clone().or_else(|| keep_url(ui_url, id));
                    let link = keep.as_ref().map_or_else(String::new, |url| format!(" · [Keep]({url})"));
                    let by = newsletter::requested_by(requesters, hide);
                    lines.push(format!("**{title}** · {} · leaves {when}{by}{link}", gib(*bytes)));
                    if let Some(url) = keep {
                        links.push((format!("Keep {title}"), url));
                    }
                    tagged.extend(requesters.iter());
                }
            }
            more(&mut lines, events.len());
            lines.push("Play it before then to keep it, or use its Keep link.".to_string());
            (format!("Leaving soon: {} title(s)", events.len()), Level::Warning)
        }
        EventKind::Deleted => {
            for event in events {
                if let Event::Deleted { title, bytes, stage, .. } = event {
                    let what = match stage {
                        Stage::Handed => "handed over for deletion",
                        Stage::Gone => "deleted",
                    };
                    lines.push(format!("**{title}** · {} · {what}", gib(*bytes)));
                }
            }
            more(&mut lines, events.len());
            (format!("Deletions: {} title(s)", events.len()), Level::Info)
        }
        EventKind::Problem => {
            for event in events {
                if let Event::Problem { message, since, .. } = event {
                    lines.push(format!("{message} (since {})", date(*since)));
                }
            }
            more(&mut lines, events.len());
            (format!("Problems: {} persisting", events.len()), Level::Failure)
        }
        EventKind::Newsletter => {
            if let Some(Event::Newsletter(letter)) = events.iter().find(|event| matches!(event, Event::Newsletter(_))) {
                return newsletter::message(letter, hide);
            }
            ("This week".to_string(), Level::Info)
        }
        EventKind::Digest => {
            let mut title = "Daily digest".to_string();
            for event in events {
                if let Event::Digest(digest) = event {
                    title = format!("Daily digest {}", utc_date(digest.day));
                    if digest.dry_run {
                        lines.push("Dry run: nothing is handed over or deleted.".to_string());
                    }
                    for disk in &digest.disks {
                        let used = disk.used_bytes as f64 / disk.capacity_bytes.max(1) as f64 * 100.0;
                        let projected = disk.projected_used_bytes as f64 / disk.capacity_bytes.max(1) as f64 * 100.0;
                        let need = match (disk.emergency, disk.target_reclaim_bytes) {
                            (true, bytes) => format!(" · emergency, must free {}", gib(bytes)),
                            (false, 0) => String::new(),
                            (false, bytes) => format!(" · must free {}", gib(bytes)),
                        };
                        lines.push(format!("`{}` {used:.0}% used, {projected:.0}% in {} days{need}", disk.volume, disk.window_days));
                    }
                    lines.push(format!("Left the library in the last day: {} title(s), {}", digest.freed_items, gib(digest.freed_bytes)));
                    if !digest.top.is_empty() {
                        lines.push("Next to go:".to_string());
                        lines.extend(
                            digest.top.iter().map(|top| format!("- {} · {} · regret {:.2}", top.title, gib(top.bytes), top.regret)),
                        );
                    }
                }
            }
            (title, Level::Info)
        }
    };
    let mentions = mentions(tagged.into_iter(), config, people);
    Message { title, lines, links, level, cards: Vec::new(), mentions }
}

/// The message a "Send test" posts.
pub(super) fn test_message(ui_url: &str) -> Message {
    let mut lines =
        vec!["This channel works: FLINCH will post Leaving Soon titles, deletions, persisting problems and the daily digest here."
            .to_string()];
    let links = if ui_url.is_empty() { Vec::new() } else { vec![("Open FLINCH".to_string(), ui_url.to_string())] };
    if ui_url.is_empty() {
        lines.push("No FLINCH address is set, so Leaving Soon messages carry no Keep link.".to_string());
    }
    Message { title: "FLINCH test notification".to_string(), lines, links, level: Level::Info, cards: Vec::new(), mentions: Vec::new() }
}

/// `text` cut to at most `max` bytes on a character boundary, marked.
fn clip(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut end = max.saturating_sub('…'.len_utf8());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

/// Characters, not bytes: Discord counts characters.
fn clip_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut clipped: String = text.chars().take(max - 1).collect();
    clipped.push('…');
    clipped
}

/// The body a channel posts for one message (ntfy's topic is added by the
/// caller, which knows the URL).
pub(super) fn body(kind: ChannelKind, message: &Message, ui_url: &str) -> Value {
    let text = message.lines.join("\n");
    match kind {
        ChannelKind::Discord => {
            let color = match message.level {
                Level::Info => 0x3B82F6,
                Level::Warning => 0xF59E0B,
                Level::Failure => 0xEF4444,
            };
            // With cards, the description leaves room in Discord's 6,000
            // characters across a message's embeds.
            let description = if message.cards.is_empty() { DISCORD_DESCRIPTION } else { DISCORD_DESCRIPTION_WITH_CARDS };
            let mut embed = json!({
                "title": clip_chars(&message.title, DISCORD_TITLE),
                "description": clip_chars(&text, description),
                "color": color,
            });
            if !ui_url.is_empty() {
                embed["url"] = Value::from(ui_url);
            }
            let mut embeds = vec![embed];
            embeds.extend(message.cards.iter().take(newsletter::MAX_CARDS).map(|card| {
                let mut embed = json!({ "title": clip_chars(&card.title, CARD_FIELD), "description": clip_chars(&card.text, CARD_FIELD), "color": color });
                if let Some(url) = &card.url {
                    embed["url"] = Value::from(url.as_str());
                }
                if let Some(image) = &card.image {
                    embed["thumbnail"] = json!({ "url": image });
                }
                embed
            }));
            // Only the listed users can be pinged; a name in a title never.
            let mentions: Vec<&String> = message.mentions.iter().take(DISCORD_MENTIONS).collect();
            let mut body = json!({ "username": "FLINCH", "embeds": embeds, "allowed_mentions": { "parse": [], "users": mentions } });
            if !mentions.is_empty() {
                body["content"] = Value::from(mentions.iter().map(|id| format!("<@{id}>")).collect::<Vec<_>>().join(" "));
            }
            body
        }
        ChannelKind::Ntfy => {
            let (priority, tags) = match message.level {
                Level::Info => (3, ["floppy_disk"]),
                Level::Warning => (4, ["hourglass"]),
                Level::Failure => (4, ["warning"]),
            };
            let actions: Vec<Value> = message
                .links
                .iter()
                .take(NTFY_ACTIONS)
                .map(|(label, url)| json!({ "action": "view", "label": clip(label, 40), "url": url, "clear": true }))
                .collect();
            let mut body = json!({
                "topic": "",
                "title": message.title,
                "message": clip(&text, NTFY_MESSAGE),
                "markdown": true,
                "priority": priority,
                "tags": tags,
                "actions": actions,
            });
            if !ui_url.is_empty() {
                body["click"] = Value::from(ui_url);
            }
            body
        }
        ChannelKind::Apprise => {
            let kind = match message.level {
                Level::Info => "info",
                Level::Warning => "warning",
                Level::Failure => "failure",
            };
            let mut body = text;
            for card in &message.cards {
                if let Some(image) = &card.image {
                    body.push_str(&format!("\n\n![{}]({image})", card.title));
                }
            }
            json!({ "title": message.title, "body": body, "type": kind, "format": "markdown" })
        }
        ChannelKind::Webhook => json!({ "source": "flinch", "test": true, "title": message.title, "text": text, "events": [] }),
    }
}

/// The webhook body: the events themselves, a Leaving Soon one with its Keep
/// link.
pub(super) fn webhook_events(events: &[&Event], ui_url: &str) -> Value {
    let events: Vec<Value> = events
        .iter()
        .map(|event| {
            let mut value = serde_json::to_value(event).unwrap_or(Value::Null);
            if let (Event::LeavingSoon { id, keep_url: None, .. }, Value::Object(fields)) = (event, &mut value) {
                if let Some(url) = keep_url(ui_url, id) {
                    fields.insert("keep_url".to_string(), Value::from(url));
                }
            }
            value
        })
        .collect();
    json!({ "source": "flinch", "events": events })
}
