//! Head-to-head harness tests: the join accounts for every line, the harness
//! reproduces FLINCH's own numbers when FLINCH is the "external" model, the
//! fitted column never sees the row it is judged on, and a live server's
//! answers are scored and recorded without leaking where it lives.

use super::*;
use crate::card::LibraryKind;
use crate::fit::export::write_predictions;
use crate::fit::panel::{build_dataset, PanelSpec};
use crate::fit::plays::Play;
use crate::fit::FitItem;
use crate::watch::WatchSource;
use rstest::rstest;

const NOW: u64 = 1_800_000_000;
const DAY: u64 = 86_400;

fn movie(index: usize) -> FitItem {
    // Every third title is played again well after it was first watched, so
    // the panel carries both outcomes at most cut dates.
    let plays: Vec<Play> = match index % 3 {
        0 => vec![NOW - 600 * DAY, NOW - 200 * DAY, NOW - 70 * DAY, NOW - 20 * DAY],
        1 => vec![NOW - 500 * DAY],
        _ => Vec::new(),
    }
    .into_iter()
    .map(|epoch| Play { epoch, episode: None, viewer: None, complete: true })
    .collect();
    FitItem {
        id: format!("radarr-{index}"),
        title: format!("Movie {index}"),
        kind: LibraryKind::Movie,
        size_bytes: (index as u64 % 7 + 1) * 3_000_000_000,
        age_days: 700.0,
        episodes_total: None,
        season_index: None,
        show_title: None,
        audience_plays: plays.clone(),
        plays,
        watch_source: Some(WatchSource::Plex),
        is_newest_season: false,
        series_status: None,
        last_aired_epoch: None,
        guid_resolved: true,
        genres: Vec::new(),
        on_disk: Vec::new(),
    }
}

fn panel() -> Vec<Example> {
    let items: Vec<FitItem> = (0..24).map(movie).collect();
    let cuts = [60.0, 150.0, 300.0];
    build_dataset(&items, &PanelSpec { now: NOW, cuts_days: &cuts, horizon_days: 30.0, tautulli_coverage_start: None })
}

#[test]
fn every_prediction_line_is_joined_or_counted_under_its_reason() {
    let dataset = panel();
    let first = &dataset[0];
    let second = &dataset[1];
    let text = [
        format!(r#"{{"id":"{}","cut_days":{},"p":0.8}}"#, first.item_id, first.cut_days),
        // The same row again: the first answer stands.
        format!(r#"{{"id":"{}","cut_days":{},"p":0.1}}"#, first.item_id, first.cut_days),
        // Right row, wrong panel date.
        format!(r#"{{"id":"{}","cut_days":{},"cut_unix":{},"p":0.5}}"#, second.item_id, second.cut_days, second.cut_unix + DAY),
        r#"{"id":"radarr-999","cut_days":60,"p":0.5}"#.to_string(),
        String::new(),
        r#"{"id":"radarr-1","cut_days":60,"p":1.5}"#.to_string(),
        r#"{"id":"radarr-1","cut_days":60}"#.to_string(),
        "not json".to_string(),
    ]
    .join("\n");
    let result = head_to_head(&dataset, &Predictions::parse(&text), NOW, 30.0);
    let join = &result.join;
    assert_eq!(join.prediction_lines, 7, "the blank line is not a prediction");
    assert_eq!((join.joined, join.duplicates, join.stale, join.unjoined, join.invalid), (1, 1, 1, 1, 3));
    assert_eq!(join.invalid_lines, vec![6, 7, 8]);
    assert_eq!(join.joined + join.duplicates + join.stale + join.unjoined + join.invalid, join.prediction_lines, "no line may vanish");
    assert_eq!(join.unanswered, dataset.len() - 1);
    assert_eq!(result.external.n, 1, "metrics cover the joined rows only");
    assert_eq!(result.priors.n, 1, "and every model is judged on those same rows");
}

#[test]
fn the_priors_answering_as_an_external_model_reproduce_the_priors_exactly() {
    // The harness's self-check: FLINCH's own answers, written in the public
    // predictions format and read back, must score exactly as the priors do.
    let dataset = panel();
    let priors = forecasts(&dataset, &ScoreWeights::default(), DEPLOYED_PRIOR_TEMPERATURE);
    let mut file = Vec::new();
    let written = write_predictions(&mut file, &dataset, &priors).expect("in-memory write");
    let text = String::from_utf8(file).expect("utf-8 JSONL");
    let result = head_to_head(&dataset, &Predictions::parse(&text), NOW, 30.0);
    assert_eq!(written, dataset.len());
    assert_eq!(result.join.joined, dataset.len());
    assert_eq!(result.join.unanswered, 0);
    let (external, reference) = (result.external, result.priors);
    assert_eq!(external.n, reference.n);
    assert_eq!(external.positives, reference.positives);
    for (name, got, want) in [
        ("auc", external.auc, reference.auc),
        ("brier", external.brier, reference.brier),
        ("log_loss", external.log_loss, reference.log_loss),
        ("ece", external.ece, reference.ece),
    ] {
        assert!((got - want).abs() < 1e-6, "{name}: {got} vs {want}");
    }
}

#[rstest]
fn an_out_of_fold_prediction_never_depends_on_its_own_outcomes(#[values(ModelKind::Recalibrated, ModelKind::Full)] kind: ModelKind) {
    // Flip every label of one item. Its own fold's fit never saw it, so its
    // out-of-fold forecast must not move by a single bit.
    let dataset = panel();
    let target = dataset[0].item_id.clone();
    let flipped: Vec<Example> = dataset
        .iter()
        .cloned()
        .map(|mut example| {
            if example.item_id == target {
                example.label = 1.0 - example.label;
            }
            example
        })
        .collect();
    let forecast = |rows: &[Example]| -> Vec<f32> { candidate::out_of_fold(kind, rows).iter().map(|row| row.forecast).collect() };
    let (before, after) = (forecast(&dataset), forecast(&flipped));
    for (row, example) in dataset.iter().enumerate().filter(|(_, example)| example.item_id == target) {
        assert_eq!(before[row].to_bits(), after[row].to_bits(), "{} leaked into its own prediction", example.item_id);
    }
    assert!(
        dataset.iter().enumerate().any(|(row, example)| example.item_id != target && before[row] != after[row]),
        "the flip must reach the other folds, or this test proves nothing"
    );
}

fn response(json: &str) -> Option<Response> {
    Some(serde_json::from_str(json).expect("a System One response"))
}

#[test]
fn server_answers_become_p_safe_and_bad_answers_stay_unanswered() {
    let dataset = panel();
    let mut responses: Vec<Option<Response>> = vec![None; dataset.len()];
    responses[0] = response(r#"{"model":"kev","answers":{"played":{"type":"noul","noul":0.2}}}"#);
    // Row 2 (index 1) failed: no response at all.
    responses[2] = response(r#"{"answers":{"played":{"type":"noul","noul":1.7}}}"#);
    responses[3] = response(r#"{"answers":{"played":{"type":"choice","choice":"yes"}}}"#);
    responses[4] = response(r#"{"answers":{"escalate":{"type":"noul","noul":0.4}}}"#);
    let predictions = Predictions::answered(&dataset, &responses);
    assert_eq!(predictions.rows.len(), 1);
    assert!((predictions.rows[0].p - 0.8).abs() < 1e-6, "P(safe) is the complement of P(played)");
    assert_eq!(predictions.invalid_lines, vec![3, 4, 5], "numbered by panel row, as in --export-panel");
    let join = head_to_head(&dataset, &predictions, NOW, 30.0).join;
    assert_eq!((join.joined, join.invalid, join.stale), (1, 3, 0), "the answer joins its own row");
    assert_eq!(join.unanswered, dataset.len() - 1, "failed and invalid rows are unanswered");
}

#[rstest]
#[case::userinfo_path_query_fragment("https://user:secret@api.typesafe.ai/v1?key=abc#token", "https://api.typesafe.ai")]
#[case::explicit_port_kept("http://kev.home:8009/", "http://kev.home:8009")]
#[case::default_port_dropped("https://api.typesafe.ai:443/v1", "https://api.typesafe.ai")]
#[case::ipv6_query("http://token@[::1]:8009?api_key=abc", "http://[::1]:8009")]
#[case::unparseable("not a url secret", "unknown")]
fn the_endpoint_keeps_only_its_origin(#[case] base_url: &str, #[case] origin: &str) {
    assert_eq!(endpoint_origin(base_url), origin);
}

#[test]
fn a_benchmark_round_trips_through_its_file() {
    let dataset = panel();
    let priors = forecasts(&dataset, &ScoreWeights::default(), DEPLOYED_PRIOR_TEMPERATURE);
    let mut file = Vec::new();
    write_predictions(&mut file, &dataset, &priors).expect("in-memory write");
    let text = String::from_utf8(file).expect("utf-8 JSONL");
    let benchmark = Benchmark {
        model: "kev-latest".to_string(),
        endpoint: "http://kev.home:8009".to_string(),
        scored_at_unix: NOW,
        result: head_to_head(&dataset, &Predictions::parse(&text), NOW, 30.0),
    };
    let dir = std::env::temp_dir().join(format!("flinch-benchmark-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    assert_eq!(read_benchmark(&dir), None, "no file, no benchmark");
    let json = serde_json::to_vec(&benchmark).expect("serialise");
    crate::persist::replace(&dir.join(BENCHMARK_FILE), &json).expect("write benchmark");
    assert_eq!(read_benchmark(&dir), Some(benchmark));
    std::fs::remove_dir_all(&dir).expect("clean up");
}
