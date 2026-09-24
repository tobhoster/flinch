//! What must hold before anything is handed over: a Maintainerr new enough to
//! understand the request, and a collection that will actually free the bytes
//! — and, for Leaving Soon, one that warns the household before it does.

use super::{CollectionInfo, MaintainerrVersion};
use crate::card::LibraryKind;
use std::collections::BTreeMap;
use std::fmt;

/// How an eviction reaches Maintainerr. Items carry it in `items.json` as
/// `"delete"` or `"leaving_soon"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Route {
    /// Straight into the kind's delete collection: the household finished it,
    /// or another copy stays.
    Delete,
    /// Announced first, in a collection Plex shows ("Leaving Soon") that acts
    /// only after its window: nobody has watched it, so the household still can.
    LeavingSoon,
}

/// The collection titles the operator configured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectionTitles {
    pub movie: String,
    pub season: String,
    /// Leaving Soon: one title for both kinds, each bound to its own Plex
    /// library. Blank sends stale evictions to the delete collections.
    pub leaving: String,
}

impl CollectionTitles {
    pub fn title(&self, kind: LibraryKind, route: Route) -> &str {
        match (route, kind) {
            (Route::LeavingSoon, _) => &self.leaving,
            (Route::Delete, LibraryKind::Movie) => &self.movie,
            (Route::Delete, LibraryKind::Season) => &self.season,
        }
    }

    /// The route an eviction takes: an announced one goes to Leaving Soon
    /// whenever the operator named that collection.
    pub fn route(&self, announced: bool) -> Route {
        if announced && !self.leaving.trim().is_empty() {
            Route::LeavingSoon
        } else {
            Route::Delete
        }
    }

    /// Titles match case-insensitively; a blank title matches nothing.
    pub(super) fn names(&self, kind: LibraryKind, route: Route, collection: &CollectionInfo) -> bool {
        let wanted = self.title(kind, route).trim();
        !wanted.is_empty() && collection.title.trim().eq_ignore_ascii_case(wanted)
    }
}

/// Why a kind's collection cannot take items.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CollectionProblem {
    NotFound,
    WrongType { found: String },
    Inactive,
    /// The collection's *arr action frees nothing (unmonitor only, or nothing),
    /// or it is an action Maintainerr cannot run for the kind.
    ArrAction { found: i64 },
    /// A Leaving Soon collection that acts on Maintainerr's next run.
    NoWarningWindow,
    /// A Leaving Soon collection Plex does not show: nobody sees the warning.
    NotShownInPlex,
}

/// A kind whose collection is unusable. Nothing of that kind is handed over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Misconfigured {
    pub kind: LibraryKind,
    pub route: Route,
    pub title: String,
    pub collection_id: Option<i64>,
    pub problem: CollectionProblem,
}

impl fmt::Display for Misconfigured {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = kind_name(self.kind);
        let label = match self.route {
            Route::Delete => "",
            Route::LeavingSoon => "Leaving Soon ",
        };
        write!(f, "{label}{kind} collection {:?}", self.title)?;
        if let Some(id) = self.collection_id {
            write!(f, " (id {id})")?;
        }
        match (&self.problem, self.route) {
            (CollectionProblem::NotFound, Route::Delete) => write!(f, " not found: create it in Maintainerr and bind a delete rule to it"),
            (CollectionProblem::NotFound, Route::LeavingSoon) => write!(
                f,
                " not found: add a rule group with this name for the {kind} library in Maintainerr, with an *arr action that deletes files, \"Take action after days\" set (14 is a good start) and \"Show on Plex home\" on; its rules may select nothing, FLINCH adds the items. Until then nothing unwatched is handed over"
            ),
            (CollectionProblem::WrongType { found }, _) => write!(f, " holds {found} items, not {kind}s"),
            (CollectionProblem::Inactive, _) => write!(f, " is inactive"),
            (CollectionProblem::ArrAction { found }, _) => match (keeps_files(*found), self.kind, *found) {
                (Some(action), _, _) => write!(f, " has the *arr action \"{action}\", which frees nothing: pick one that deletes files"),
                (None, LibraryKind::Season, 1) => write!(
                    f,
                    " has the *arr action \"Unmonitor and delete all\", which Maintainerr refuses for seasons, so nothing would ever leave: pick \"Unmonitor and delete existing episodes\""
                ),
                (None, _, _) => write!(f, " has arrAction {found}, which frees nothing for a {kind}; allowed: {:?}", allowed_arr_actions(self.kind)),
            },
            (CollectionProblem::NoWarningWindow, _) => {
                write!(f, " acts on Maintainerr's next run: set \"Take action after days\" (14 is a good start) so the household can react")
            }
            (CollectionProblem::NotShownInPlex, _) => write!(
                f,
                " is not shown in Plex: turn on \"Show on Plex home\" (or library recommended) and leave \"Keep in Maintainerr only\" off"
            ),
        }
    }
}

fn kind_name(kind: LibraryKind) -> &'static str {
    match kind {
        LibraryKind::Movie => "movie",
        LibraryKind::Season => "season",
    }
}

/// ServarrAction values that delete files and that Maintainerr runs for the
/// kind: for movies delete (0) and unmonitor and delete all (1); for seasons
/// delete (0), unmonitor and delete existing (2) and delete the show when it
/// empties (5). Maintainerr 3.29 refuses unmonitor and delete all (1) for a
/// season ("not supported for type: season"), so its members would never leave.
fn allowed_arr_actions(kind: LibraryKind) -> &'static [i64] {
    match kind {
        LibraryKind::Movie => &[0, 1],
        LibraryKind::Season => &[0, 2, 5],
    }
}

/// The Maintainerr UI's name for a ServarrAction that keeps the files, so a
/// refusal names what the operator sees, not a number.
fn keeps_files(action: i64) -> Option<&'static str> {
    match action {
        3 | 6 => Some("Unmonitor … keep files"),
        4 => Some("Do nothing"),
        7 => Some("Change quality profile and search"),
        _ => None,
    }
}

/// Whether a collection can take items of `kind` by `route`: it frees their
/// bytes, and a Leaving Soon one also warns first — a real window, shown in Plex.
pub(super) fn validate_collection(collection: &CollectionInfo, kind: LibraryKind, route: Route) -> Result<(), CollectionProblem> {
    if !collection.media_type.trim().eq_ignore_ascii_case(kind_name(kind)) {
        return Err(CollectionProblem::WrongType { found: collection.media_type.clone() });
    }
    if !collection.is_active {
        return Err(CollectionProblem::Inactive);
    }
    if !allowed_arr_actions(kind).contains(&collection.arr_action) {
        return Err(CollectionProblem::ArrAction { found: collection.arr_action });
    }
    if route == Route::LeavingSoon {
        if collection.delete_after_days.unwrap_or(0) < 1 {
            return Err(CollectionProblem::NoWarningWindow);
        }
        if collection.keep_in_maintainerr_only || !(collection.visible_on_home || collection.visible_on_recommended) {
            return Err(CollectionProblem::NotShownInPlex);
        }
    }
    Ok(())
}

/// Every collection carrying the title for `kind` and `route`, provided all of
/// them are valid. With one collection per Plex section the title can repeat,
/// and any invalid one makes the whole kind misconfigured for that route.
pub(super) fn resolve<'a>(
    collections: &'a [CollectionInfo],
    titles: &CollectionTitles,
    kind: LibraryKind,
    route: Route,
) -> Result<Vec<&'a CollectionInfo>, Vec<Misconfigured>> {
    let named: Vec<&CollectionInfo> = collections
        .iter()
        .filter(|c| titles.names(kind, route, c) && (route == Route::Delete || serves(c, kind)))
        .collect();
    let misconfigured =
        |collection_id, problem| Misconfigured { kind, route, title: titles.title(kind, route).to_string(), collection_id, problem };
    if named.is_empty() {
        return Err(vec![misconfigured(None, CollectionProblem::NotFound)]);
    }
    let problems: Vec<Misconfigured> = named
        .iter()
        .filter_map(|c| validate_collection(c, kind, route).err().map(|problem| misconfigured(Some(c.id), problem)))
        .collect();
    if problems.is_empty() {
        Ok(named)
    } else {
        Err(problems)
    }
}

/// Leaving Soon shares one title across kinds, so each same-titled collection
/// counts toward the kind whose library it can serve: a movie collection is
/// the movie one, any other type the TV one — where a show-typed collection is
/// reported as the wrong type, without blocking movies.
fn serves(collection: &CollectionInfo, kind: LibraryKind) -> bool {
    let movie = collection.media_type.trim().eq_ignore_ascii_case(kind_name(LibraryKind::Movie));
    match kind {
        LibraryKind::Movie => movie,
        LibraryKind::Season => !movie,
    }
}

/// The one collection bound to the item's Plex section. Without a known
/// section, a kind with a single collection is used; several are ambiguous.
pub(super) fn for_section<'a>(candidates: &[&'a CollectionInfo], section: Option<u32>) -> Option<&'a CollectionInfo> {
    let mut matching = candidates.iter().copied().filter(|c| match section {
        Some(section) => c.library_id.trim() == section.to_string(),
        None => true,
    });
    let first = matching.next()?;
    matching.next().is_none().then_some(first)
}

/// A collection FLINCH hands items to, as the status page needs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Destination {
    pub route: Route,
    /// Days Maintainerr waits after the hand-off before it acts.
    pub window_days: Option<i64>,
}

/// Every collection that carries a configured title, by id: the route it
/// serves and its window. Validity is not required — a member of a collection
/// that broke since still leaves on that collection's schedule.
pub fn destinations(collections: &[CollectionInfo], titles: &CollectionTitles) -> BTreeMap<i64, Destination> {
    collections
        .iter()
        .filter_map(|collection| {
            let kind = [LibraryKind::Movie, LibraryKind::Season]
                .into_iter()
                .find(|kind| collection.media_type.trim().eq_ignore_ascii_case(kind_name(*kind)))?;
            let route = [Route::LeavingSoon, Route::Delete].into_iter().find(|route| titles.names(kind, *route, collection))?;
            Some((collection.id, Destination { route, window_days: collection.delete_after_days }))
        })
        .collect()
}

/// Advice on the collections FLINCH hands items to: settings that do not
/// block a hand-over but leave cleanup undone after Maintainerr deletes. With
/// Seerr configured in Maintainerr and "Force delete Seerr request" off, the
/// request stays until Seerr's availability sync notices, and the title cannot
/// be requested again until then.
pub(super) fn cleanup_warnings(collections: &[CollectionInfo], titles: &CollectionTitles, seerr_configured: bool) -> Vec<String> {
    if !seerr_configured {
        return Vec::new();
    }
    let handed_to = destinations(collections, titles);
    collections
        .iter()
        .filter(|collection| handed_to.contains_key(&collection.id) && !collection.force_seerr)
        .map(|collection| {
            format!(
                "{} collection {:?} (id {}) leaves Seerr requests behind: turn on \"Force delete Seerr request\" so a removed title can be requested again at once",
                collection.media_type.trim().to_ascii_lowercase(),
                collection.title,
                collection.id
            )
        })
        .collect()
}

/// Whether this Maintainerr takes handovers at all. Exclusions are always
/// allowed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Handover {
    Allowed { version: String },
    /// Below v3.10 the collection add is not validated and takes a different
    /// body, so nothing is handed over.
    Refused { version: String },
}

pub(super) fn handover(version: &MaintainerrVersion) -> Handover {
    let label = version.to_string();
    match version {
        MaintainerrVersion::Release { major, minor, .. } if (*major, *minor) < (3, 10) => Handover::Refused { version: label },
        MaintainerrVersion::Release { .. } | MaintainerrVersion::Branch(_) => Handover::Allowed { version: label },
    }
}

impl Handover {
    pub fn version(&self) -> &str {
        match self {
            Self::Allowed { version } | Self::Refused { version } => version,
        }
    }
}

impl fmt::Display for Handover {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Allowed { version } => write!(f, "Maintainerr {version}: handover allowed"),
            Self::Refused { version } => write!(f, "Maintainerr {version} is older than 3.10: nothing is handed over"),
        }
    }
}
