use super::link::{expiry, token_id, url, Action, Link, LinkError, LinkSecret, Signer};
use super::*;
use crate::capacity::{CapacityForecast, VolumeForecast};
use crate::plan::knapsack::Force;
use crate::plan::{generate_eviction_plan, PlannerConfig};
use crate::regret::Regret;
use rstest::rstest;

const GIB: u64 = 1 << 30;
const NOW: u64 = 1_800_000_000;
const SECRET: &[u8] = b"0123456789abcdef0123456789abcdef";

fn secret() -> LinkSecret {
    LinkSecret::new(SECRET).expect("a 32-byte secret")
}

fn link(card: &str, action: Action) -> Link {
    Link { card_id: card.to_string(), action, expires: NOW + 7 * DAY, by: "Ann".to_string() }
}

fn candidate(id: &str, p_watch: f64) -> MediaCandidate {
    let regret = Regret::new(p_watch, 1.0, 1.0);
    MediaCandidate {
        id: id.to_string(),
        title: id.to_string(),
        size_bytes: 10 * GIB,
        volume: Some("movies".to_string()),
        regret,
        reason: String::new(),
        age_days: 400.0,
        exclusion: None,
        sequence: None,
        handed: false,
        announce: false,
        protect: false,
        quality: crate::quality::advise(&regret, &crate::quality::Item::default()),
        eviction_safety: 0.0,
        force: None,
    }
}

fn forecast(target_gib: u64) -> VolumeForecast {
    VolumeForecast {
        volume: "movies".to_string(),
        forecast: CapacityForecast {
            current_used_bytes: 0,
            max_capacity_bytes: 1,
            current_utilization: 0.0,
            daily_ingest_rate_bytes: 0,
            queue_bytes: 0,
            in_flight_bytes: 0,
            projected_used_bytes: 0,
            target_reclaim_bytes: target_gib * GIB,
            is_emergency: false,
        },
    }
}

/// A book after `link` was clicked at `NOW`.
fn clicked(link: &Link) -> (RequestBook, String) {
    let token = secret().sign(link);
    let id = token_id(&token).expect("a token id");
    let mut book = RequestBook::default();
    book.record(link, &id, "Alien", &HouseholdConfig::default(), NOW).expect("recorded");
    (book, id)
}

#[test]
fn a_signed_link_verifies_to_exactly_what_was_signed() {
    let wanted = Link { by: "Zoë, with a\nline break".to_string(), ..link("sonarr-12-s3", Action::Remove) };
    let token = secret().sign(&wanted);
    assert!(token.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte)), "a token is URL-safe: {token}");
    assert_eq!(secret().verify(&token, NOW), Ok(Link { by: "Zoë, with aline break".to_string(), ..wanted }));
}

#[rstest]
#[case::a_second_before(NOW + 7 * DAY - 1, Ok("radarr-7".to_string()))]
#[case::at_its_second(NOW + 7 * DAY, Err(LinkError::Expired))]
#[case::long_after(NOW + 30 * DAY, Err(LinkError::Expired))]
fn a_link_dies_at_its_expiry(#[case] now: u64, #[case] verified: Result<String, LinkError>) {
    let token = secret().sign(&link("radarr-7", Action::Keep));
    assert_eq!(secret().verify(&token, now).map(|link| link.card_id), verified);
}

#[rstest]
#[case::another_card(|token: &str| {
    let (_, tag) = token.split_once('.').unwrap_or_default();
    let forged = Link { card_id: "radarr-8".to_string(), ..link("radarr-7", Action::Keep) };
    let (payload, _) = LinkSecret::new(b"another secret of thirty-two bytes!").map(|other| other.sign(&forged)).unwrap_or_default().split_once('.').map(|(p, t)| (p.to_string(), t.to_string())).unwrap_or_default();
    format!("{payload}.{tag}")
})]
#[case::remove_for_keep(|token: &str| token.replacen('.', "A.", 1))]
#[case::cut_mac(|token: &str| token[..token.len() - 2].to_string())]
#[case::no_mac(|token: &str| token.split('.').next().unwrap_or_default().to_string())]
#[case::garbage(|_: &str| "not a token at all".to_string())]
#[case::huge(|token: &str| token.repeat(20))]
fn a_tampered_or_foreign_token_is_refused(#[case] tamper: fn(&str) -> String) {
    let token = secret().sign(&link("radarr-7", Action::Keep));
    assert_eq!(secret().verify(&tamper(&token), NOW), Err(LinkError::Invalid));
}

#[test]
fn a_token_from_another_secret_and_a_short_secret_are_refused() {
    let other = LinkSecret::new(b"ffffffffffffffffffffffffffffffff").expect("32 bytes");
    assert_eq!(secret().verify(&other.sign(&link("radarr-7", Action::Keep)), NOW), Err(LinkError::Invalid));
    assert!(LinkSecret::new(b"too short").is_none());
    assert_eq!(format!("{:?}", secret()), "LinkSecret(32 bytes)", "the secret is never printed");
}

#[test]
fn a_link_is_the_same_all_day_and_ends_a_day_after_the_leave_date() {
    let day = NOW - NOW % DAY;
    assert_eq!(expiry(None, day + 60, 14), expiry(None, day + DAY - 1, 14));
    assert_eq!(expiry(Some(day + 3 * DAY + 5), NOW, 14), day + 5 * DAY);
    assert!(expiry(Some(NOW + 400 * DAY), NOW, 14) <= NOW + 91 * DAY, "never past the cap");
    let signer = Signer { secret: &secret(), ui_url: "https://flinch.example/", days: 14, now: NOW };
    let made = signer.link("radarr-7", Action::Keep, None, "household").expect("a link");
    assert!(made.starts_with("https://flinch.example/r/"), "{made}");
    assert_eq!(made, Signer { now: NOW + 60, ..signer }.link("radarr-7", Action::Keep, None, "household").expect("a link"));
    assert_eq!(url("", "t"), None, "no address, no link");
}

#[test]
fn a_replayed_link_finds_its_request_and_changes_nothing() {
    let keep = link("radarr-7", Action::Keep);
    let (mut book, id) = clicked(&keep);
    let before = book.clone();
    let again = book.record(&keep, &id, "Alien", &HouseholdConfig::default(), NOW + DAY).expect("replayed");
    assert!(matches!(again, Recorded::Existing(_)));
    assert_eq!(book, before);
}

#[test]
fn a_requested_keep_pins_until_it_expires() {
    let (mut book, _) = clicked(&link("radarr-7", Action::Keep));
    let until = NOW + 60 * DAY;
    assert_eq!(book.pins(until - 1).get("radarr-7"), Some(&until));
    assert!(book.pins(until).is_empty(), "a keep ends at its end");

    let mut candidates =
        vec![candidate("radarr-7", 0.0), MediaCandidate { exclusion: Some(Exclusion::NotInPlex), ..candidate("radarr-8", 0.0) }];
    let pins = HashMap::from([("radarr-7", until), ("radarr-8", until)]);
    assert_eq!(pin(&mut candidates, &pins), 2);
    assert!(candidates.iter().all(|c| c.protect && c.exclusion == Some(Exclusion::Pinned(Pin::Requested { until }))));

    book.prune(until);
    assert_eq!(book.requests[0].status, Status::Expired);
    book.prune(until + 90 * DAY);
    assert!(book.requests.is_empty(), "an expired keep is forgotten after 90 days");
}

#[test]
fn a_favorite_stays_the_reason_a_requested_keep_joins() {
    let mut favorite = vec![MediaCandidate { exclusion: Some(Exclusion::Pinned(Pin::Favorite)), ..candidate("radarr-7", 0.0) }];
    pin(&mut favorite, &HashMap::from([("radarr-7", NOW)]));
    assert_eq!(favorite[0].exclusion, Some(Exclusion::Pinned(Pin::Favorite)));
}

#[test]
fn an_approved_removal_becomes_a_must_evict_rule_the_planner_honours() {
    let (mut book, id) = clicked(&link("dear", Action::Remove));
    assert_eq!(book.requests[0].status, Status::Pending);
    assert!(book.rules(NOW).is_empty(), "a pending removal does nothing");

    assert_eq!(book.decide(&id, Decision::Approve, NOW).map(|request| request.status), Ok(Status::Approved));
    let rules = book.rules(NOW);
    assert_eq!(rules.len(), 1);
    assert_eq!((rules[0].effect, rules[0].scope.ids.clone()), (Effect::MustEvict, vec!["dear".to_string()]));

    let mut candidates = vec![candidate("cheap", 0.1), candidate("dear", 0.9)];
    crate::rules::apply(&rules, &mut candidates, &HashMap::new(), NOW);
    assert_eq!((candidates[0].force, candidates[1].force), (None, Some(Force::Must)));
    let plan = generate_eviction_plan(&candidates, &[forecast(10)], &PlannerConfig::default()).expect("valid inputs");
    assert_eq!(plan.items.iter().map(|item| item.id.as_str()).collect::<Vec<_>>(), vec!["dear"], "the approved title goes first");
}

#[test]
fn keep_beats_remove() {
    let (mut book, removal) = clicked(&link("dear", Action::Remove));
    book.decide(&removal, Decision::Approve, NOW).expect("approved");
    let keep = link("dear", Action::Keep);
    let token = secret().sign(&Link { by: "Bo".to_string(), ..keep.clone() });
    book.record(&keep, &token_id(&token).expect("id"), "Dear", &HouseholdConfig::default(), NOW).expect("kept");
    assert!(book.rules(NOW).is_empty(), "an active keep silences the approved removal");

    // Even a removal rule that slipped through loses to the pin.
    let mut candidates = vec![candidate("cheap", 0.1), candidate("dear", 0.9)];
    pin(&mut candidates, &book.pins(NOW));
    let forced = [Rule {
        name: "removal".to_string(),
        enabled: true,
        scope: Scope { ids: vec!["dear".to_string()], ..Scope::default() },
        effect: Effect::MustEvict,
    }];
    crate::rules::apply(&forced, &mut candidates, &HashMap::new(), NOW);
    assert_eq!(candidates[1].force, None);
    assert!(matches!(candidates[1].exclusion, Some(Exclusion::Pinned(Pin::Requested { .. }))));
    let plan = generate_eviction_plan(&candidates, &[forecast(10)], &PlannerConfig::default()).expect("valid inputs");
    assert_eq!(plan.items.iter().map(|item| item.id.as_str()).collect::<Vec<_>>(), vec!["cheap"]);

    // Once the keep ends, the approval stands again.
    assert_eq!(book.rules(NOW + 60 * DAY).len(), 1);
}

#[rstest]
#[case::deny_pending(Action::Remove, None, Decision::Deny, Ok(Status::Denied))]
#[case::cancel_approved(Action::Remove, Some(Decision::Approve), Decision::Cancel, Ok(Status::Cancelled))]
#[case::cancel_keep(Action::Keep, None, Decision::Cancel, Ok(Status::Cancelled))]
#[case::approve_a_keep(Action::Keep, None, Decision::Approve, Err(Refused::Settled))]
#[case::deny_twice(Action::Remove, Some(Decision::Deny), Decision::Deny, Err(Refused::Settled))]
fn the_admin_decides_only_what_is_open(
    #[case] action: Action,
    #[case] first: Option<Decision>,
    #[case] decision: Decision,
    #[case] outcome: Result<Status, Refused>,
) {
    let (mut book, id) = clicked(&link("radarr-7", action));
    if let Some(first) = first {
        book.decide(&id, first, NOW).expect("the first decision");
    }
    assert_eq!(book.decide(&id, decision, NOW).map(|request| request.status), outcome);
    assert_eq!(book.decide("nope", decision, NOW).map(|request| request.status), Err(Refused::Unknown));
}

#[test]
fn a_second_removal_link_finds_the_queued_one_and_removal_can_be_switched_off() {
    let (mut book, _) = clicked(&link("radarr-7", Action::Remove));
    let other = secret().sign(&Link { by: "Bo".to_string(), ..link("radarr-7", Action::Remove) });
    let again = book.record(&link("radarr-7", Action::Remove), &token_id(&other).expect("id"), "Alien", &HouseholdConfig::default(), NOW);
    assert!(matches!(again, Ok(Recorded::Existing(_))));
    assert_eq!(book.requests.len(), 1);

    let off = HouseholdConfig { allow_remove: false, ..HouseholdConfig::default() };
    let refused = RequestBook::default().record(&link("radarr-7", Action::Remove), "id", "Alien", &off, NOW);
    assert_eq!(refused, Err(Refused::RemoveOff));
}

#[test]
fn an_open_removal_of_a_title_that_left_is_settled() {
    let (mut book, _) = clicked(&link("radarr-7", Action::Remove));
    book.forget_gone(&HashSet::from(["radarr-8"]), NOW);
    assert_eq!(book.requests[0].status, Status::Cancelled);
}

#[test]
fn the_shelf_summary_lists_soonest_first_with_each_link_under_the_dates_line() {
    let items = [
        ("Later".to_string(), NOW + 9 * DAY, "https://f/r/b".to_string()),
        ("Sooner".to_string(), NOW + 2 * DAY, "https://f/r/a".to_string()),
    ];
    let summary = shelf_summary(Some("Leaves between Oct 14 and Oct 23"), &items);
    let lines: Vec<&str> = summary.lines().collect();
    assert_eq!(lines.len(), 4);
    assert_eq!(lines[0], "Leaves between Oct 14 and Oct 23");
    assert!(lines[2].starts_with("• Sooner (leaves after ") && lines[2].ends_with("https://f/r/a"), "{summary}");
    assert!(lines[3].starts_with("• Later"), "{summary}");
    assert_eq!(shelf_summary(None, &items).lines().count(), 3);
}
