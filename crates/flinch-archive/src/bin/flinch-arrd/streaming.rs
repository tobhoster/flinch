//! Streaming availability for the cycle ([`flinch_archive::signals::streaming`]):
//! a budgeted trickle of TMDB watch-provider lookups for the library's titles
//! and the open Seerr requests, cached in `streaming.json`. Off unless the
//! operator switches it on; a missing key or a failed lookup leaves titles
//! unknown, and an unknown title gets no discount.

use flinch_archive::arr::{ArrMovie, ArrSeries};
use flinch_archive::signals::streaming::{self, tmdb, StreamingCache, StreamingConfig, StreamingStatus, Title};
use flinch_archive::signals::Signals;

/// Look up what is due and fill `signals.streams`; `None` while off.
pub(super) async fn gather(
    http: &reqwest::Client,
    config: &StreamingConfig,
    (movies, series): (&[ArrMovie], &[ArrSeries]),
    now: u64,
    signals: &mut Signals,
) -> Option<StreamingStatus> {
    if !config.enabled {
        return None;
    }
    let key = std::env::var(&config.tmdb_key_env).unwrap_or_default();
    let key = key.trim();
    let path = super::state_dir().join("streaming.json");
    let mut cache: StreamingCache = super::read_state(&path);
    let mut looked_up = 0;
    if key.is_empty() {
        signals.problems.push(format!("TMDB key not set ({} is empty): streaming uses earlier lookups only", config.tmdb_key_env));
    } else {
        let mut titles: Vec<Title> = streaming::card_titles(movies, series).into_values().collect();
        titles.sort_unstable();
        titles.dedup();
        titles.extend(signals.requests.iter().filter_map(|request| Title::of_media(&request.media)));
        let refreshed = tmdb::refresh(http, tmdb::TMDB_BASE, key, &config.region, &mut cache, &titles, now).await;
        if let Some(error) = refreshed.problem {
            // TmdbError never carries the URL, which may hold the v3 key.
            eprintln!("[flinch-arrd] streaming: TMDB lookup failed: {error}");
            signals.problems.push(format!("TMDB lookup failed ({error}): streaming uses earlier lookups only"));
        }
        looked_up = refreshed.looked_up;
        if looked_up > 0 {
            super::write_state(&path, &cache);
        }
    }
    signals.streams = cache.streams(&config.region, &config.provider_ids);
    Some(StreamingStatus { region: config.region.clone(), known: cache.known(&config.region), streaming: signals.streams.len(), looked_up })
}
