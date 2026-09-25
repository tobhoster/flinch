//! Plex GUIDs → external catalogue ids, and how two id sets relate.
//!
//! Modern agents list every external id as a `Guid` element (`tmdb://603`,
//! `tvdb://81189`, `imdb://tt0133093`); legacy agents put one in the item's
//! `guid` attribute (`com.plexapp.agents.imdb://tt0133093?lang=en`). A `plex://`
//! GUID names Plex's own catalogue and says nothing the *arrs can match, but it
//! outlives a library re-add, so plays join by it (see [`super::migration`]).

use crate::ids::ExternalIds;

/// One external id a GUID names.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CatalogueId {
    Tmdb(u32),
    Tvdb(u32),
    Imdb(String),
}

/// Parse one GUID. Legacy GUIDs with a path (`thetvdb://81189/1/2`, an episode)
/// name a child of the id, not the item itself, and are ignored.
pub fn parse(guid: &str) -> Option<CatalogueId> {
    let (scheme, rest) = guid.split_once("://")?;
    let value = rest.split('?').next()?;
    if value.is_empty() || value.contains('/') {
        return None;
    }
    match scheme.rsplit('.').next()? {
        "tmdb" | "themoviedb" => value.parse().ok().map(CatalogueId::Tmdb),
        "tvdb" | "thetvdb" => value.parse().ok().map(CatalogueId::Tvdb),
        "imdb" => imdb(value).map(CatalogueId::Imdb),
        _ => None,
    }
}

/// An IMDb id in canonical form (`tt` + digits, lowercase), or `None`.
pub fn imdb(value: &str) -> Option<String> {
    let lower = value.trim().to_ascii_lowercase();
    let digits = lower.strip_prefix("tt")?;
    (!digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit())).then_some(lower)
}

/// Plex's own catalogue GUID for a `kind` item (`plex://movie/5d77…`,
/// `plex://episode/…`), trimmed. Unlike a ratingKey it survives the item
/// being removed and added again. An agent GUID never counts: an episode's
/// names its show, and a legacy agent's is replaced by a re-match.
pub fn plex_guid<'a>(guid: &'a str, kind: &str) -> Option<&'a str> {
    let guid = guid.trim();
    let id = guid.strip_prefix("plex://")?.strip_prefix(kind)?.strip_prefix('/')?;
    (!id.is_empty() && !id.contains(['/', '?'])).then_some(guid)
}

/// Every id a set of GUIDs names. The first id per catalogue wins.
pub fn external_ids<'a>(guids: impl IntoIterator<Item = &'a str>) -> ExternalIds {
    let mut ids = ExternalIds::default();
    for id in guids.into_iter().filter_map(parse) {
        match id {
            CatalogueId::Tmdb(tmdb) => {
                ids.tmdb.get_or_insert(tmdb);
            }
            CatalogueId::Tvdb(tvdb) => {
                ids.tvdb.get_or_insert(tvdb);
            }
            CatalogueId::Imdb(imdb) => {
                ids.imdb.get_or_insert(imdb);
            }
        }
    }
    ids
}

/// Every id in a set, for indexing.
pub fn catalogue_ids(ids: &ExternalIds) -> Vec<CatalogueId> {
    let mut out = Vec::with_capacity(3);
    out.extend(ids.tmdb.map(CatalogueId::Tmdb));
    out.extend(ids.tvdb.map(CatalogueId::Tvdb));
    out.extend(ids.imdb.as_deref().and_then(imdb).map(CatalogueId::Imdb));
    out
}

/// How two id sets describe one item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Relation {
    /// They share an id and disagree on none: the same item.
    Same,
    /// Some catalogue gives each a different id: different items, whatever the
    /// title says.
    Conflict,
    /// No catalogue is known on both sides: ids cannot decide.
    Unknown,
}

pub fn relate(a: &ExternalIds, b: &ExternalIds) -> Relation {
    let imdb_a = a.imdb.as_deref().and_then(imdb);
    let imdb_b = b.imdb.as_deref().and_then(imdb);
    let verdicts =
        [a.tmdb.zip(b.tmdb).map(|(x, y)| x == y), a.tvdb.zip(b.tvdb).map(|(x, y)| x == y), imdb_a.zip(imdb_b).map(|(x, y)| x == y)];
    if verdicts.contains(&Some(false)) {
        Relation::Conflict
    } else if verdicts.contains(&Some(true)) {
        Relation::Same
    } else {
        Relation::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    #[case::modern_tmdb("tmdb://603", Some(CatalogueId::Tmdb(603)))]
    #[case::modern_tvdb("tvdb://81189", Some(CatalogueId::Tvdb(81189)))]
    #[case::modern_imdb("imdb://tt0133093", Some(CatalogueId::Imdb("tt0133093".into())))]
    #[case::legacy_imdb("com.plexapp.agents.imdb://tt0111161?lang=en", Some(CatalogueId::Imdb("tt0111161".into())))]
    #[case::legacy_tmdb("com.plexapp.agents.themoviedb://1234?lang=en", Some(CatalogueId::Tmdb(1234)))]
    #[case::legacy_tvdb_show("com.plexapp.agents.thetvdb://81189?lang=en", Some(CatalogueId::Tvdb(81189)))]
    // An episode's legacy GUID names the show plus a path: not the item's id.
    #[case::legacy_tvdb_episode("com.plexapp.agents.thetvdb://81189/1/2?lang=en", None)]
    #[case::plex_catalogue("plex://movie/5d7768ba96b655001fdc0408", None)]
    #[case::not_an_imdb_id("imdb://nm0000206", None)]
    #[case::garbage("tmdb://abc", None)]
    fn guids_parse_to_the_catalogue_id_they_name(#[case] guid: &str, #[case] expected: Option<CatalogueId>) {
        assert_eq!(parse(guid), expected);
    }

    #[rstest]
    #[case::movie("plex://movie/5d7768ba96b655001fdc0408", "movie", Some("plex://movie/5d7768ba96b655001fdc0408"))]
    #[case::episode_padded(" plex://episode/5d9c0a0c ", "episode", Some("plex://episode/5d9c0a0c"))]
    #[case::other_kind("plex://episode/5d9c0a0c", "movie", None)]
    #[case::kind_prefix_only("plex://movies/5d77", "movie", None)]
    #[case::no_id("plex://movie/", "movie", None)]
    #[case::legacy_agent("com.plexapp.agents.imdb://tt0113277?lang=en", "movie", None)]
    #[case::external_id("tmdb://949", "movie", None)]
    fn only_plex_s_own_guid_of_the_kind_asked_for_counts(#[case] guid: &str, #[case] kind: &str, #[case] expected: Option<&str>) {
        assert_eq!(plex_guid(guid, kind), expected);
    }

    fn ids(tmdb: Option<u32>, imdb: Option<&str>) -> ExternalIds {
        ExternalIds { tmdb, tvdb: None, imdb: imdb.map(str::to_string) }
    }

    #[rstest]
    #[case::shared_id(ids(Some(1), None), ids(Some(1), Some("tt9")), Relation::Same)]
    #[case::imdb_case_insensitive(ids(None, Some("TT0133093")), ids(None, Some("tt0133093")), Relation::Same)]
    #[case::one_disagreement_vetoes(ids(Some(1), Some("tt9")), ids(Some(1), Some("tt8")), Relation::Conflict)]
    #[case::different_remake(ids(Some(1924), None), ids(Some(1061474), None), Relation::Conflict)]
    #[case::nothing_in_common(ids(Some(1), None), ids(None, Some("tt9")), Relation::Unknown)]
    fn id_sets_are_the_same_item_only_on_a_shared_id_and_no_disagreement(
        #[case] a: ExternalIds,
        #[case] b: ExternalIds,
        #[case] expected: Relation,
    ) {
        assert_eq!(relate(&a, &b), expected);
        assert_eq!(relate(&b, &a), expected, "the relation is symmetric");
    }
}
