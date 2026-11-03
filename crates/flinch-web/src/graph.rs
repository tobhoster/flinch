//! `GET /api/graph`: the library as a graph, for the Graph tab. One join of
//! what the daemon already published, nothing fetched and nothing written:
//!
//! - items (`items.json`) with their path, tags and requests (`plan-inputs.json`
//!   facts), joined to the disk each one is on (`STAGED_ON`);
//! - who played each one (`tautulli.json`, `WATCHED_BY`), by the item's own
//!   Plex keys and episode GUIDs, never by title, with ignored viewers left out;
//! - who requested it (`REQUESTED_BY`);
//! - what holds it (`BLOCKED_BY_TAG`): the *arr keep tag, the operator's own
//!   keeps (`operator-keeps.json`), or an exclusion FLINCH placed itself.
//!
//! A file the daemon has not written yet reads as empty: the graph is then
//! smaller, never an error.

use crate::AppState;
use axum::extract::State as AxumState;
use axum::response::{IntoResponse, Response};
use flinch_archive::daemon::read_settings;
use flinch_archive::rules::preview::{PlanInputs, INPUTS_FILE};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;

pub(crate) const WATCHED_BY: &str = "WATCHED_BY";
pub(crate) const REQUESTED_BY: &str = "REQUESTED_BY";
pub(crate) const STAGED_ON: &str = "STAGED_ON";
pub(crate) const BLOCKED_BY_TAG: &str = "BLOCKED_BY_TAG";

/// The fields of an `items.json` card the graph needs.
#[derive(Deserialize, Default)]
#[serde(default)]
struct Card {
    id: String,
    title: String,
    kind: String,
    size_bytes: u64,
    p_watch: Option<f64>,
    regret: Option<f64>,
    decision: String,
    route: Option<String>,
    volume: Option<String>,
    quality: Option<String>,
    reason: String,
    protected: bool,
    leaves_at: Option<u64>,
    last_watched_days: Option<f64>,
    advice: Option<Advice>,
    play_keys: Option<PlayKeys>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Advice {
    explanation: String,
    action: AdviceAction,
}

/// What the quality advice says to do: keep, downgrade or eligible for eviction.
#[derive(Deserialize, Default)]
#[serde(default)]
struct AdviceAction {
    #[serde(rename = "type")]
    kind: String,
}

/// How Plex knows the item: a movie's keys, or a season's show and season
/// keys with its episodes' GUIDs (which survive a re-added library).
#[derive(Deserialize, Default)]
#[serde(default)]
struct PlayKeys {
    rating_keys: Vec<String>,
    season_rating_keys: Vec<String>,
    show_rating_keys: Vec<String>,
    season: Option<u32>,
    episode_guids: Vec<String>,
}

/// One Tautulli stream as the daemon persisted it (strings, as Tautulli sends them).
#[derive(Deserialize, Default)]
#[serde(default)]
struct Stream {
    rating_key: String,
    parent_rating_key: String,
    grandparent_rating_key: String,
    parent_media_index: String,
    guid: String,
    user: String,
}

impl PlayKeys {
    fn played_in(&self, stream: &Stream) -> bool {
        let has = |keys: &[String], key: &str| !key.is_empty() && keys.iter().any(|known| known == key);
        has(&self.rating_keys, &stream.rating_key)
            || has(&self.season_rating_keys, &stream.parent_rating_key)
            || has(&self.episode_guids, &stream.guid)
            || (has(&self.show_rating_keys, &stream.grandparent_rating_key)
                && self.season.is_some_and(|season| stream.parent_media_index.parse() == Ok(season)))
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Status {
    capacity: Option<Capacity>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Capacity {
    target_utilization: Option<f64>,
    volumes: Vec<VolumeStatus>,
}

#[derive(Deserialize, Serialize, Default, Clone)]
#[serde(default)]
pub(crate) struct VolumeStatus {
    path: String,
    utilization: f64,
    used_bytes: u64,
    capacity_bytes: u64,
    target_reclaim_bytes: u64,
}

#[derive(Serialize)]
pub(crate) struct Item {
    id: String,
    title: String,
    kind: String,
    size_bytes: u64,
    p_watch: Option<f64>,
    regret: Option<f64>,
    decision: String,
    route: Option<String>,
    volume: Option<String>,
    quality: Option<String>,
    reason: String,
    explanation: Option<String>,
    /// The quality advice's action (`keep`, `downgrade`, `eligible_for_eviction`).
    advice: Option<String>,
    leaves_at: Option<u64>,
    last_watched_days: Option<f64>,
    path: Option<String>,
    tags: Vec<String>,
}

#[derive(Serialize)]
pub(crate) struct User {
    name: String,
    plays: usize,
}

#[derive(Serialize)]
pub(crate) struct Guard {
    id: &'static str,
    label: String,
}

#[derive(Serialize, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub(crate) struct Edge {
    source: String,
    target: String,
    kind: &'static str,
}

#[derive(Serialize)]
pub(crate) struct Graph {
    items: Vec<Item>,
    users: Vec<User>,
    disks: Vec<VolumeStatus>,
    guards: Vec<Guard>,
    edges: Vec<Edge>,
    target_utilization: Option<f64>,
    /// Which joins had data this cycle: an absent source is said, so a graph
    /// without watch edges never reads as "nobody watched anything".
    sources: Sources,
}

#[derive(Serialize)]
struct Sources {
    tautulli: bool,
    facts: bool,
}

fn read<T: for<'de> Deserialize<'de>>(path: &Path) -> Option<T> {
    std::fs::read(path).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok())
}

pub(crate) fn item_node(id: &str) -> String {
    format!("item:{id}")
}

pub(crate) fn build(dir: &Path) -> Graph {
    let cards: Vec<Card> = read(&dir.join("items.json")).unwrap_or_default();
    let inputs: Option<PlanInputs> = read(&dir.join(INPUTS_FILE));
    let streams: Option<Vec<Stream>> = read(&dir.join("tautulli.json"));
    let status: Status = read(&dir.join("status.json")).unwrap_or_default();
    let operator_keeps: BTreeSet<String> = read(&dir.join("operator-keeps.json")).unwrap_or_default();
    let settings = read_settings(&dir.join("settings.json")).unwrap_or_default();
    let ignored: HashSet<String> = settings.ignore_viewers.iter().map(|name| name.to_lowercase()).collect();
    let keep_tag = settings.keep_tag.trim().to_lowercase();
    let facts = inputs.as_ref().map(|inputs| &inputs.facts);

    let capacity = status.capacity.unwrap_or_default();
    let disks = capacity.volumes;
    let known_disks: HashSet<&str> = disks.iter().map(|disk| disk.path.as_str()).collect();

    let mut edges: BTreeSet<Edge> = BTreeSet::new();
    let mut plays: BTreeMap<String, usize> = BTreeMap::new();
    let mut guards: BTreeMap<&'static str, String> = BTreeMap::new();
    let mut items = Vec::with_capacity(cards.len());

    for card in cards {
        let node = item_node(&card.id);
        let fact = facts.and_then(|facts| facts.get(&card.id));
        if let Some(volume) = card.volume.as_deref().filter(|volume| known_disks.contains(volume)) {
            edges.insert(Edge { source: node.clone(), target: format!("disk:{volume}"), kind: STAGED_ON });
        }
        if let (Some(keys), Some(streams)) = (&card.play_keys, &streams) {
            for stream in streams.iter().filter(|stream| !stream.user.is_empty() && !ignored.contains(&stream.user.to_lowercase())) {
                if keys.played_in(stream) {
                    *plays.entry(stream.user.clone()).or_default() += 1;
                    edges.insert(Edge { source: node.clone(), target: format!("user:{}", stream.user), kind: WATCHED_BY });
                }
            }
        }
        for request in fact.and_then(|fact| fact.requests.as_ref()).into_iter().flatten() {
            plays.entry(request.requester.clone()).or_default();
            edges.insert(Edge { source: node.clone(), target: format!("user:{}", request.requester), kind: REQUESTED_BY });
        }
        let tags: Vec<String> = fact.and_then(|fact| fact.tags.clone()).unwrap_or_default();
        let mut guard = |id: &'static str, label: String| {
            guards.entry(id).or_insert(label);
            edges.insert(Edge { source: node.clone(), target: format!("guard:{id}"), kind: BLOCKED_BY_TAG });
        };
        if !keep_tag.is_empty() && tags.iter().any(|tag| tag.to_lowercase() == keep_tag) {
            guard("keep_tag", format!("*arr tag {}", settings.keep_tag.trim()));
        }
        if operator_keeps.contains(&card.id) {
            guard("operator_keep", "Your keep (Maintainerr exclusion or keep label)".to_string());
        }
        if card.protected {
            guard("flinch_exclusion", "FLINCH exclusion (kept while it is not picked)".to_string());
        }
        items.push(Item {
            id: card.id,
            title: card.title,
            kind: card.kind,
            size_bytes: card.size_bytes,
            p_watch: card.p_watch,
            regret: card.regret,
            decision: card.decision,
            route: card.route,
            volume: card.volume,
            quality: card.quality,
            reason: card.reason,
            advice: card.advice.as_ref().map(|advice| advice.action.kind.clone()).filter(|kind| !kind.is_empty()),
            explanation: card.advice.map(|advice| advice.explanation).filter(|text| !text.is_empty()),
            leaves_at: card.leaves_at,
            last_watched_days: card.last_watched_days,
            path: fact.and_then(|fact| fact.path.clone()),
            tags,
        });
    }

    Graph {
        items,
        users: plays.into_iter().map(|(name, plays)| User { name, plays }).collect(),
        disks,
        guards: guards.into_iter().map(|(id, label)| Guard { id, label }).collect(),
        edges: edges.into_iter().collect(),
        target_utilization: capacity.target_utilization,
        sources: Sources { tautulli: streams.is_some(), facts: facts.is_some() },
    }
}

pub(crate) async fn api_graph(AxumState(st): AxumState<AppState>) -> Response {
    let dir = st.dir.clone();
    match tokio::task::spawn_blocking(move || build(&dir)).await {
        Ok(graph) => axum::Json(graph).into_response(),
        Err(error) => crate::refuse(axum::http::StatusCode::INTERNAL_SERVER_ERROR, &format!("graph failed: {error}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{get, scratch, send, state, TOKEN};
    use axum::http::StatusCode;
    use std::fs;

    fn write(dir: &Path, name: &str, body: &str) {
        fs::write(dir.join(name), body).unwrap();
    }

    fn edges_of(graph: &Graph, kind: &str) -> Vec<(String, String)> {
        graph.edges.iter().filter(|edge| edge.kind == kind).map(|edge| (edge.source.clone(), edge.target.clone())).collect()
    }

    /// The live shapes: a movie and a season, a Tautulli stream for each by a
    /// different key (movie rating key, episode GUID after a re-add), a guest
    /// the household ignores, a request, the keep tag and an operator keep.
    fn library(dir: &Path) {
        write(
            dir,
            "items.json",
            r#"[
              {"id":"radarr-1","title":"Heat","kind":"movie","size_bytes":10,"decision":"delete","volume":"/data","protected":true,
               "play_keys":{"kind":"movie","rating_keys":["828"]}},
              {"id":"sonarr-2-s1","title":"Carrie S1","kind":"season","size_bytes":20,"decision":"keep","volume":"/data/media/tv-b",
               "play_keys":{"kind":"season","show_rating_keys":["1549"],"season_rating_keys":["1550"],"season":1,"episode_guids":["plex://episode/a"]}},
              {"id":"sonarr-3-s1","title":"Nobody S1","kind":"season","size_bytes":30,"decision":"keep","volume":"/nowhere"}
            ]"#,
        );
        write(
            dir,
            "tautulli.json",
            r#"[
              {"rating_key":"828","user":"ann"},
              {"rating_key":"9999","parent_rating_key":"7777","guid":"plex://episode/a","user":"bo"},
              {"rating_key":"828","user":"Guest"},
              {"rating_key":"1","parent_rating_key":"2","grandparent_rating_key":"1549","parent_media_index":"2","user":"cy"}
            ]"#,
        );
        write(
            dir,
            "plan-inputs.json",
            r#"{"computed_at":0,"forecasts":[],"candidates":[],"facts":{
              "radarr-1":{"path":"/data/media/movies/Heat (1995)","tags":["Flinch-Keep"],"requests":[{"requester":"dee"}]}
            }}"#,
        );
        write(dir, "operator-keeps.json", r#"["sonarr-2-s1"]"#);
        write(dir, "settings.json", r#"{"keep_tag":"flinch-keep","ignore_viewers":["guest"]}"#);
        write(
            dir,
            "status.json",
            r#"{"capacity":{"target_utilization":0.75,"volumes":[{"path":"/data","utilization":0.74},{"path":"/data/media/tv-b","utilization":0.936}]}}"#,
        );
    }

    #[test]
    fn items_join_to_viewers_requesters_disks_and_guards_by_identity() {
        let tmp = scratch("graph");
        fs::create_dir_all(&tmp).unwrap();
        library(&tmp);
        let graph = build(&tmp);

        assert_eq!(
            edges_of(&graph, WATCHED_BY),
            [("item:radarr-1".into(), "user:ann".into()), ("item:sonarr-2-s1".into(), "user:bo".into())],
            "by rating key and by episode GUID; an ignored guest and another season of the show are no play"
        );
        assert_eq!(edges_of(&graph, REQUESTED_BY), [("item:radarr-1".into(), "user:dee".into())]);
        assert_eq!(
            edges_of(&graph, STAGED_ON),
            [("item:radarr-1".into(), "disk:/data".into()), ("item:sonarr-2-s1".into(), "disk:/data/media/tv-b".into())],
            "an item on a disk no longer governed links to nothing"
        );
        assert_eq!(
            edges_of(&graph, BLOCKED_BY_TAG),
            [
                ("item:radarr-1".into(), "guard:flinch_exclusion".into()),
                ("item:radarr-1".into(), "guard:keep_tag".into()),
                ("item:sonarr-2-s1".into(), "guard:operator_keep".into()),
            ],
            "the keep tag matches case-insensitively"
        );
        let users: Vec<(&str, usize)> = graph.users.iter().map(|user| (user.name.as_str(), user.plays)).collect();
        assert_eq!(users, [("ann", 1), ("bo", 1), ("dee", 0)]);
        assert_eq!(graph.items[0].path.as_deref(), Some("/data/media/movies/Heat (1995)"));
        assert!(graph.sources.tautulli && graph.sources.facts);
        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn before_the_first_cycle_the_graph_is_empty_and_says_what_is_missing() {
        let tmp = scratch("graph-empty");
        fs::create_dir_all(&tmp).unwrap();
        let graph = build(&tmp);
        assert!(graph.items.is_empty() && graph.edges.is_empty());
        assert!(!graph.sources.tautulli && !graph.sources.facts);
        fs::remove_dir_all(&tmp).ok();
    }

    #[tokio::test]
    async fn the_graph_is_behind_the_login() {
        let tmp = scratch("graph-auth");
        let st = state(&tmp, Some(TOKEN));
        assert_eq!(send(&st, get("/api/graph", &[])).await.status(), StatusCode::UNAUTHORIZED);
        let auth = format!("Bearer {TOKEN}");
        assert_eq!(send(&st, get("/api/graph", &[("authorization", auth.as_str())])).await.status(), StatusCode::OK);
        fs::remove_dir_all(&tmp).ok();
    }
}
