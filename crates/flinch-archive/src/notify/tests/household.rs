//! The household's notifications: who a requester is, what the shared and
//! the personal messages say, and that each person is told once on their
//! own addresses.

use super::{client, finished, scratch, serve, NOW, UI};
use crate::notify::newsletter::{due, week_of, NewsItem, Newsletter};
use crate::notify::recipients::{resolve, tmdb_poster, Recipient};
use crate::notify::{ChannelConfig, ChannelKind, Event, EventKind, HouseholdNotify, Notifier, NotifyConfig, Personal, RecipientOverride};
use crate::signals::seerr::Contact;
use rstest::rstest;

const DAY: u64 = 86_400;
const ANN_DISCORD: &str = "123456789012345678";

fn contacts() -> Vec<Contact> {
    vec![
        Contact { name: "Ann".to_string(), email: Some("ann@example.org".to_string()), aliases: vec!["annplex".to_string()] },
        Contact { name: "Bo".to_string(), email: Some("bo@example.org".to_string()), aliases: Vec::new() },
        Contact { name: "Di".to_string(), email: None, aliases: Vec::new() },
    ]
}

fn household() -> HouseholdNotify {
    HouseholdNotify {
        enabled: true,
        ntfy_server: "https://ntfy.example/".to_string(),
        apprise_api: "http://apprise:8000".to_string(),
        email_url_env: "FLINCH_TEST_HOUSEHOLD_MAILTO".to_string(),
        email_seerr_users: true,
        recipients: vec![
            RecipientOverride {
                user: "annplex".to_string(),
                discord_id: ANN_DISCORD.to_string(),
                ntfy_topic: "ann".to_string(),
                ..RecipientOverride::default()
            },
            RecipientOverride {
                user: "Cy".to_string(),
                apprise_env: "FLINCH_TEST_CY".to_string(),
                newsletter: false,
                ..RecipientOverride::default()
            },
            RecipientOverride { user: "Bo".to_string(), muted: true, ..RecipientOverride::default() },
        ],
        ..HouseholdNotify::default()
    }
}

#[test]
fn a_requester_is_found_by_any_seerr_name_with_overrides_on_top() {
    let people = resolve(&household(), &contacts());
    let ann = Recipient {
        name: "Ann".to_string(),
        discord_id: Some(ANN_DISCORD.to_string()),
        ntfy_topic: Some("ann".to_string()),
        apprise_env: None,
        email: Some("ann@example.org".to_string()),
        newsletter: true,
    };
    let cy = Recipient { name: "Cy".to_string(), apprise_env: Some("FLINCH_TEST_CY".to_string()), ..Recipient::default() };
    assert_eq!(people, vec![ann, cy], "Bo is muted, Di has nowhere to be told");

    let no_seerr_mail = HouseholdNotify { email_seerr_users: false, ..household() };
    assert_eq!(resolve(&no_seerr_mail, &contacts())[0].email, None, "Seerr's emails only when asked for");
    assert!(household().validate().is_ok());
}

#[rstest]
#[case::tmdb_original("https://image.tmdb.org/t/p/original/abc.jpg", Some("https://image.tmdb.org/t/p/w342/abc.jpg"))]
#[case::tmdb_sized("https://image.tmdb.org/t/p/w500/x_y-1.png", Some("https://image.tmdb.org/t/p/w342/x_y-1.png"))]
#[case::plex_with_token("http://plex:32400/library/metadata/1/thumb?X-Plex-Token=abc", None)]
#[case::arr_local("/MediaCover/1/poster.jpg", None)]
#[case::tvdb("https://artworks.thetvdb.com/banners/posters/1.jpg", None)]
#[case::query_smuggled("https://image.tmdb.org/t/p/original/abc.jpg?token=1", None)]
fn only_tmdb_posters_reach_a_message(#[case] url: &str, #[case] poster: Option<&str>) {
    assert_eq!(tmdb_poster(url).as_deref(), poster);
}

fn leaving(id: &str, requesters: &[&str]) -> Event {
    Event::LeavingSoon {
        id: id.to_string(),
        title: format!("Title {id}"),
        bytes: 4 << 30,
        handed_at: NOW,
        leaves_at: Some(NOW + 7 * DAY),
        requesters: requesters.iter().map(|name| name.to_string()).collect(),
        poster: Some("https://image.tmdb.org/t/p/w342/p.jpg".to_string()),
        keep_url: Some(format!("{UI}r/token-{id}")),
    }
}

fn config(kind: ChannelKind, url: String, household: HouseholdNotify) -> NotifyConfig {
    let events = vec![EventKind::LeavingSoon, EventKind::Newsletter];
    let channel = ChannelConfig { name: "home".to_string(), kind, url, events, ..ChannelConfig::default() };
    NotifyConfig { channels: vec![channel], ui_url: UI.to_string(), household, ..NotifyConfig::default() }
}

#[rstest]
#[case::named_and_mentioned(false)]
#[case::hidden(true)]
#[tokio::test]
async fn a_shared_discord_message_names_and_mentions_the_requester_unless_hidden(#[case] hide: bool) {
    let dir = scratch(&format!("household-discord-{hide}"));
    let (base, server) = serve(vec![204]);
    let config = config(ChannelKind::Discord, format!("{base}/hook"), HouseholdNotify { hide_requester: hide, ..household() });
    let people = resolve(&config.household, &contacts());
    let http = client();
    let sent = Notifier::new(&http, &config, &dir).with_recipients(&people).send_at(&[leaving("radarr-1", &["Ann"])], NOW).await;
    let taken = finished(server);

    assert_eq!(sent.messages, 1);
    let body = &taken[0].body;
    let text = body["embeds"][0]["description"].as_str().unwrap_or_default();
    assert!(text.contains(&format!("[Keep]({UI}r/token-radarr-1)")), "the household's own link: {text}");
    assert_eq!(text.contains("requested by Ann"), !hide, "{text}");
    let mention = format!("<@{ANN_DISCORD}>");
    assert_eq!(body["content"].as_str() == Some(mention.as_str()), !hide);
    let users = body["allowed_mentions"]["users"].as_array().map(Vec::len).unwrap_or_default();
    assert_eq!(users, usize::from(!hide));
}

fn newsletter() -> Newsletter {
    let item = |id: &str, title: &str, leaves: u64| NewsItem {
        id: id.to_string(),
        title: title.to_string(),
        bytes: 2 << 30,
        leaves_at: Some(NOW + leaves * DAY),
        poster: Some(format!("https://image.tmdb.org/t/p/w342/{id}.jpg")),
        requesters: vec!["Ann".to_string()],
        keep_url: Some(format!("{UI}r/keep-{id}")),
        remove_url: None,
    };
    Newsletter {
        week: week_of(NOW),
        leaving: vec![item("radarr-1", "Alien", 2), item("radarr-2", "Heat", 5)],
        yours: vec![NewsItem { remove_url: Some(format!("{UI}r/remove-radarr-3")), keep_url: None, ..item("radarr-3", "Up", 0) }],
        left_items: 3,
        left_bytes: 30 << 30,
    }
}

#[test]
fn the_newsletter_is_due_once_a_week_from_its_day_and_hour() {
    let monday = (week_of(NOW) * 7 - 3) * DAY;
    assert_eq!(week_of(monday), week_of(monday + 7 * DAY - 1));
    assert!(!due(monday + 4 * DAY + 16 * 3_600, 4, 17), "Friday before five");
    assert!(due(monday + 4 * DAY + 17 * 3_600, 4, 17));
    assert!(due(monday + 6 * DAY, 4, 17), "a daemon down on Friday sends it on Sunday");
    assert!(!due(monday + 7 * DAY, 4, 17), "the next week starts over");
}

#[tokio::test]
async fn the_newsletter_shows_posters_keep_links_and_your_titles_to_remove() {
    let dir = scratch("household-newsletter");
    let (base, server) = serve(vec![200, 204]);
    let apprise = config(ChannelKind::Apprise, format!("{base}/notify/flinch"), household());
    let discord = config(ChannelKind::Discord, format!("{base}/hook"), household());
    let http = client();
    let event = [Event::Newsletter(newsletter())];
    Notifier::new(&http, &apprise, &dir).send_at(&event, NOW).await;
    Notifier::new(&http, &discord, &scratch("household-newsletter-discord")).send_at(&event, NOW).await;
    let taken = finished(server);

    let body = taken[0].body["body"].as_str().unwrap_or_default();
    let alien = body.find("**Alien**").unwrap_or(usize::MAX);
    assert!(alien < body.find("**Heat**").unwrap_or(0), "soonest first: {body}");
    assert!(body.contains(&format!("[Keep]({UI}r/keep-radarr-1)")) && body.contains("requested by Ann"), "{body}");
    assert!(body.contains(&format!("[Remove]({UI}r/remove-radarr-3)")), "{body}");
    assert!(body.contains("![Alien](https://image.tmdb.org/t/p/w342/radarr-1.jpg)"), "{body}");
    assert!(body.contains("Left the library this week: 3 title(s), 30.0 GiB"), "{body}");
    assert!(!body.contains("plex"), "never a Plex URL");

    let embeds = taken[1].body["embeds"].as_array().cloned().unwrap_or_default();
    assert_eq!(embeds.len(), 3, "the newsletter and one card per leaving title");
    assert_eq!(embeds[1]["thumbnail"]["url"], "https://image.tmdb.org/t/p/w342/radarr-1.jpg");
    assert_eq!(embeds[1]["url"], format!("{UI}r/keep-radarr-1"));
}

#[tokio::test]
async fn a_requester_is_told_once_on_each_of_their_own_addresses() {
    std::env::set_var("FLINCH_TEST_HOUSEHOLD_MAILTO", "mailtos://flinch:pw@smtp.example?from=flinch@example.org");
    let dir = scratch("household-personal");
    let (base, server) = serve(vec![200, 200]);
    let household = HouseholdNotify { ntfy_server: format!("{base}/"), apprise_api: base.clone(), ..household() };
    let config = NotifyConfig { ui_url: UI.to_string(), household, ..NotifyConfig::default() };
    let ann = resolve(&config.household, &contacts()).remove(0);
    let people = [Personal { recipient: ann, events: vec![leaving("radarr-1", &["Ann"])] }];
    let http = client();
    let notifier = Notifier::new(&http, &config, &dir);

    let first = notifier.send_personal(&people, NOW).await;
    let again = notifier.send_personal(&people, NOW + 3_600).await;
    let taken = finished(server);

    assert_eq!((first.messages, again.messages), (2, 0), "{:?}", first.failures);
    assert_eq!((taken[0].path.as_str(), taken[0].body["topic"].as_str()), ("/", Some("ann")));
    assert!(taken[0].body["title"].as_str().unwrap_or_default().ends_with("you asked for"));
    assert_eq!(taken[1].path, "/notify/");
    let urls = taken[1].body["urls"].as_str().unwrap_or_default();
    assert_eq!(urls, "mailtos://flinch:pw@smtp.example?from=flinch@example.org&to=ann@example.org");
}
