//! The Leaving Soon shelf as people read it in Plex: soonest-leaving first,
//! with the window's dates in the collection summary, so the warning carries
//! a date without opening FLINCH (Maintainerr hub #40 "order by days left",
//! #75 "days left in the collection"). Pure text and order; the writes are in
//! [`super::collections`].

const DAY: u64 = 86_400;
const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/// "Oct 23" for a Unix time (UTC, like the rest of FLINCH's dates).
pub fn month_day(epoch: u64) -> String {
    let date = crate::notify::utc_date(epoch / DAY);
    let mut parts = date.split('-').skip(1).filter_map(|part| part.parse::<usize>().ok());
    match (parts.next(), parts.next()) {
        (Some(month @ 1..=12), Some(day)) => format!("{} {day}", MONTHS[month - 1]),
        _ => date,
    }
}

/// The summary's first line: "Leaves between Oct 14 and Oct 23", or "Leaves
/// Oct 23" when every item leaves the same day; `None` for an empty shelf.
pub fn dates_line(untils: &[u64]) -> Option<String> {
    let first = month_day(*untils.iter().min()?);
    let last = month_day(*untils.iter().max()?);
    Some(if first == last { format!("Leaves {first}") } else { format!("Leaves between {first} and {last}") })
}

/// Member ratingKeys in shelf order: soonest leave date first, then title,
/// so the order is stable across cycles. `members` are (ratingKey, until,
/// title).
pub fn shelf_order(members: &[(String, u64, String)]) -> Vec<String> {
    let mut sorted: Vec<&(String, u64, String)> = members.iter().collect();
    sorted.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.2.cmp(&b.2)).then_with(|| a.0.cmp(&b.0)));
    sorted.into_iter().map(|(key, ..)| key.clone()).collect()
}

/// The `move` calls that turn `current` into `wanted` (python-plexapi
/// `Collection.moveItem(item, after)`): `(ratingKey, after)`, `after = None`
/// for the top. Members FLINCH does not track keep their relative order below
/// its own; an already-ordered prefix costs no call.
pub fn moves(current: &[String], wanted: &[String]) -> Vec<(String, Option<String>)> {
    let ours: Vec<&String> = current.iter().filter(|key| wanted.contains(key)).collect();
    let wanted: Vec<&String> = wanted.iter().filter(|key| current.contains(key)).collect();
    let kept = ours.iter().zip(&wanted).take_while(|(a, b)| a == b).count();
    if kept == wanted.len() && current.iter().take(kept).eq(wanted.iter().copied().take(kept)) {
        return Vec::new();
    }
    // A foreign member above ours means even the prefix must move to the top.
    let start = if current.iter().take(kept).eq(wanted.iter().copied().take(kept)) { kept } else { 0 };
    (start..wanted.len()).map(|i| (wanted[i].clone(), i.checked_sub(1).map(|prev| wanted[prev].clone()))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    const OCT_14: u64 = 1_791_936_000; // 2026-10-14 UTC
    const OCT_23: u64 = OCT_14 + 9 * DAY;

    #[rstest]
    #[case::span(&[OCT_23, OCT_14 + 3_600], Some("Leaves between Oct 14 and Oct 23"))]
    #[case::one_day(&[OCT_23, OCT_23 + 60], Some("Leaves Oct 23"))]
    #[case::empty(&[], None)]
    fn the_summary_names_the_window(#[case] untils: &[u64], #[case] line: Option<&str>) {
        assert_eq!(dates_line(untils).as_deref(), line);
    }

    #[test]
    fn soonest_leaves_first_then_by_title() {
        let members = [
            ("3".to_string(), OCT_23, "B".to_string()),
            ("1".to_string(), OCT_14, "Z".to_string()),
            ("2".to_string(), OCT_23, "A".to_string()),
        ];
        assert_eq!(shelf_order(&members), ["1", "2", "3"]);
    }

    fn keys(list: &[&str]) -> Vec<String> {
        list.iter().map(|key| key.to_string()).collect()
    }

    #[rstest]
    #[case::ordered(&["1", "2", "x"], &["1", "2"], &[])]
    #[case::tail_swapped(&["1", "3", "2"], &["1", "2", "3"], &[("2", Some("1")), ("3", Some("2"))])]
    #[case::foreign_on_top(&["x", "1", "2"], &["1", "2"], &[("1", None), ("2", Some("1"))])]
    #[case::reversed(&["2", "1"], &["1", "2"], &[("1", None), ("2", Some("1"))])]
    fn only_out_of_place_members_move(#[case] current: &[&str], #[case] wanted: &[&str], #[case] expected: &[(&str, Option<&str>)]) {
        let expected: Vec<(String, Option<String>)> =
            expected.iter().map(|(key, after)| (key.to_string(), after.map(str::to_string))).collect();
        assert_eq!(moves(&keys(current), &keys(wanted)), expected);
    }
}
