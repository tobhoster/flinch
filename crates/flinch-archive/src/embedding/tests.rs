use super::*;
use crate::arr::{ArrCollection, ArrLanguage, ArrMovie, ArrSeries};
use crate::plex::{PlexMetadata, PlexTag};
use rstest::rstest;

fn tags(names: &[&str]) -> Vec<PlexTag> {
    names.iter().map(|name| PlexTag { tag: name.to_string() }).collect()
}

fn matrix() -> ArrMovie {
    ArrMovie {
        id: 3,
        title: "The Matrix".into(),
        year: Some(1999),
        overview: Some("A hacker  learns\nthe truth.".into()),
        genres: vec!["Action".into(), "Science Fiction".into()],
        certification: Some("R".into()),
        studio: Some("Warner Bros.".into()),
        runtime: Some(136),
        original_language: Some(ArrLanguage { name: "English".into() }),
        collection: Some(ArrCollection { title: "The Matrix Collection".into() }),
        ..Default::default()
    }
}

fn matrix_plex() -> PlexMetadata {
    PlexMetadata {
        rating_key: "42".into(),
        tagline: Some("Welcome to the Real World.".into()),
        genres: tags(&["Action", "Sci-Fi"]),
        directors: tags(&["Lana Wachowski", "Lilly Wachowski"]),
        writers: tags(&["Lilly Wachowski", "Lana Wachowski"]),
        roles: tags(&["Keanu Reeves", "Laurence Fishburne", "Carrie-Anne Moss"]),
        countries: tags(&["United States of America"]),
        ..Default::default()
    }
}

#[test]
fn what_the_household_did_never_reaches_the_text() {
    let quiet = movie_text(&matrix(), Some(&matrix_plex()));
    let mut movie = matrix();
    movie.added = Some("2024-01-01T00:00:00Z".into());
    movie.has_file = true;
    movie.size_on_disk = 9_000_000_000;
    movie.tags = vec![7];
    movie.monitored = Some(false);
    movie.keep = true;
    let mut plex = matrix_plex();
    plex.view_count = Some(4);
    plex.last_viewed_at = Some(1_700_000_000);
    plex.viewed_at = Some(1_700_000_000);
    plex.account_id = Some(2);
    assert_eq!(movie_text(&movie, Some(&plex)), quiet, "plays, dates, files and tags leave the text as it was");
}

#[test]
fn the_text_names_the_title_and_its_content_behind_the_classification_prompt() {
    let text = movie_text(&matrix(), Some(&matrix_plex()));
    assert!(text.starts_with("task: classification | query: Title: The Matrix\nYear: 1999\nKind: movie\n"), "{text}");
    for part in [
        "Genres: Action, Sci-Fi, Science Fiction",
        "Collection: The Matrix Collection",
        "Directed by: Lana Wachowski, Lilly Wachowski",
        "Tagline: Welcome to the Real World.",
        "Overview: A hacker learns the truth.",
    ] {
        assert!(text.contains(part), "{part:?} missing from {text}");
    }
}

#[test]
fn the_order_a_server_lists_things_in_never_changes_the_text() {
    let mut shuffled = matrix();
    shuffled.genres.reverse();
    shuffled.genres.push("action".into());
    let mut plex = matrix_plex();
    plex.roles.reverse();
    plex.directors.reverse();
    let text = movie_text(&matrix(), Some(&matrix_plex()));
    assert_eq!(movie_text(&shuffled, Some(&plex)), text);
    assert_eq!(text_hash(&movie_text(&shuffled, Some(&plex))), text_hash(&text));
}

#[test]
fn the_arr_text_stands_alone_and_a_long_overview_is_cut() {
    let show = ArrSeries {
        id: 7,
        title: "Long Show".into(),
        series_type: "anime".into(),
        overview: Some("word ".repeat(1_000)),
        ..Default::default()
    };
    let text = series_text(&show, None);
    assert!(text.contains("Kind: series\nSeries type: anime"), "{text}");
    assert_eq!(text.chars().count(), 2_000);
}

#[rstest]
#[case::season("sonarr-7-s2", "sonarr-7")]
#[case::specials("sonarr-7-s0", "sonarr-7")]
#[case::movie("radarr-3", "radarr-3")]
#[case::show("sonarr-7", "sonarr-7")]
fn seasons_share_their_shows_subject(#[case] card: &str, #[case] subject: &str) {
    assert_eq!(subject_of(card), subject);
}

#[rstest]
#[case::truncated_then_normalised(&[3.0, 4.0, 100.0], 2, Some(vec![0.6, 0.8]))]
#[case::exact_length(&[0.0, 2.0], 2, Some(vec![0.0, 1.0]))]
#[case::too_short(&[1.0], 2, None)]
#[case::nan_in_the_prefix(&[1.0, f32::NAN, 0.0], 2, None)]
#[case::zero_prefix(&[0.0, 0.0, 1.0], 2, None)]
fn a_matryoshka_prefix_is_renormalised_or_refused(#[case] raw: &[f32], #[case] dimensions: usize, #[case] wanted: Option<Vec<f32>>) {
    assert_eq!(truncate(raw, dimensions), wanted);
}

/// The published checkpoint's layout: the audio tower, the projections, the
/// text model, then the vision tower; `text_gap` splits the text tensors.
fn checkpoint_header(text_gap: bool) -> Vec<u8> {
    let gap = u64::from(text_gap);
    serde_json::to_vec(&serde_json::json!({
        "__metadata__": {"format": "pt"},
        "audio_tower.w": {"dtype": "BF16", "shape": [2], "data_offsets": [0, 4]},
        "embed_vision.w": {"dtype": "BF16", "shape": [2], "data_offsets": [4, 8]},
        "language_model.b": {"dtype": "BF16", "shape": [3], "data_offsets": [14 + gap, 20 + gap]},
        "language_model.a": {"dtype": "BF16", "shape": [3], "data_offsets": [8, 14]},
        "vision_tower.b": {"dtype": "BF16", "shape": [1], "data_offsets": [24 + gap, 26 + gap]},
        "vision_tower.a": {"dtype": "BF16", "shape": [2], "data_offsets": [20 + gap, 24 + gap]},
    }))
    .unwrap_or_default()
}

fn offsets(subset: &weights::Subset) -> serde_json::Value {
    let header: serde_json::Value = serde_json::from_slice(&subset.header).unwrap_or_default();
    let entries = header.as_object().map(|entries| entries.iter().map(|(name, entry)| (name.clone(), entry["data_offsets"].clone())));
    serde_json::Value::Object(entries.into_iter().flatten().collect())
}

#[test]
fn the_text_tensors_are_cut_out_as_one_range_with_offsets_from_zero() {
    let Ok(text) = weights::subset(&checkpoint_header(false), weights::TEXT_TENSORS) else { panic!("text tensors") };
    assert_eq!(text.ranges, vec![(8, 20)]);
    assert_eq!(text.header.len() % 8, 0, "the data must start 8-byte aligned");
    assert_eq!(offsets(&text), serde_json::json!({"language_model.a": [0, 6], "language_model.b": [6, 12]}));
}

#[test]
fn the_vision_tensors_are_two_ranges_around_the_text_model_laid_end_to_end() {
    let Ok(vision) = weights::subset(&checkpoint_header(false), weights::VISION_TENSORS) else { panic!("vision tensors") };
    assert_eq!(vision.ranges, vec![(4, 8), (20, 26)], "the projection, then the tower; never the text model between");
    assert_eq!(vision.header.len() % 8, 0);
    assert_eq!(offsets(&vision), serde_json::json!({"embed_vision.w": [0, 4], "vision_tower.a": [4, 8], "vision_tower.b": [8, 10]}));
}

#[test]
fn a_tower_split_by_another_is_fetched_as_two_ranges() {
    let Ok(text) = weights::subset(&checkpoint_header(true), weights::TEXT_TENSORS) else { panic!("text tensors") };
    assert_eq!(text.ranges, vec![(8, 14), (15, 21)]);
    assert_eq!(offsets(&text), serde_json::json!({"language_model.a": [0, 6], "language_model.b": [6, 12]}));
}

#[rstest]
#[case::overlapping(serde_json::json!({"vision_tower.a": {"data_offsets": [0, 4]}, "vision_tower.b": {"data_offsets": [2, 6]}}))]
#[case::absent(serde_json::json!({"language_model.a": {"data_offsets": [0, 4]}}))]
#[case::no_offsets(serde_json::json!({"vision_tower.a": {"dtype": "BF16"}}))]
fn an_unusable_vision_header_is_refused(#[case] header: serde_json::Value) {
    assert!(weights::subset(header.to_string().as_bytes(), weights::VISION_TENSORS).is_err());
}

#[rstest]
#[case::tmdb_original_poster(3000, 2000, (960, 624))]
#[case::small_poster_is_enlarged(281, 190, (960, 624))]
#[case::square(1000, 1000, (768, 768))]
#[case::landscape(1080, 1920, (576, 1056))]
#[case::sliver(40, 4000, (48, 8016))]
fn a_poster_is_resized_to_the_largest_48_pixel_grid_in_the_budget(#[case] height: u32, #[case] width: u32, #[case] size: (u32, u32)) {
    let wanted = Preprocessing::default();
    assert_eq!(wanted.target_size(height, width).ok(), Some(size));
    let (rows, columns) = (size.0 as usize / poster::PATCH, size.1 as usize / poster::PATCH);
    assert!(rows * columns <= wanted.max_soft_tokens * 9 && rows % 3 == 0 && columns % 3 == 0);
}

#[test]
fn a_poster_becomes_row_major_patches_of_row_column_channel_values() {
    let image = image::RgbImage::from_fn(624, 960, |x, y| image::Rgb([(x % 256) as u8, (y % 256) as u8, 51]));
    let Ok(patches) = Preprocessing::default().patches(&image) else { panic!("a 624×960 image needs no resize") };
    assert_eq!((patches.rows, patches.columns, patches.len(), patches.soft_tokens()), (60, 39, 2340, 260));
    assert_eq!(patches.pixels.len(), 2340 * poster::PATCH_VALUES);
    let value = |at: usize| (patches.pixels[at] * 255.0).round() as u32;
    assert_eq!([value(0), value(1), value(2)], [0, 0, 51], "patch 0 starts at pixel (0, 0)");
    assert_eq!([value(3), value(4)], [1, 0], "then the next pixel of its first row");
    assert_eq!([value(48), value(49)], [0, 1], "then its second row");
    assert_eq!([value(768), value(769)], [16, 0], "patch 1 is the next 16 columns");
    assert_eq!([value(39 * 768), value(39 * 768 + 1)], [0, 16], "patch 39 starts the second patch row");
    assert_eq!(patches.positions().nth(40), Some((1, 1)));
}

#[rstest]
#[case::pinned_revision(serde_json::json!(false), 280, true)]
#[case::normalising_processor(serde_json::json!(true), 280, false)]
#[case::untrained_budget(serde_json::json!(false), 300, false)]
fn only_the_image_processor_this_port_implements_is_accepted(
    #[case] normalize: serde_json::Value,
    #[case] budget: u32,
    #[case] accepted: bool,
) {
    let dir = scratch(&format!("processor-{budget}-{normalize}"));
    let config = serde_json::json!({"image_processor": {
        "do_normalize": normalize, "do_rescale": true, "do_resize": true, "max_soft_tokens": budget,
        "patch_size": 16, "pooling_kernel_size": 3, "resample": 3, "rescale_factor": 0.00392156862745098,
    }});
    std::fs::write(dir.join("processor_config.json"), config.to_string()).expect("write");
    let read = Preprocessing::read(&dir.join("processor_config.json"));
    assert_eq!(read.is_ok(), accepted, "{read:?}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn the_poster_placeholder_follows_the_title_and_stays_the_only_one() {
    let text = with_poster("task: classification | query: Title: The <|image|>Matrix\nYear: 1999");
    assert_eq!(text, "task: classification | query: Title: The Matrix\nPoster: <|image|>\nYear: 1999");
    assert_ne!(poster_hash(&text, "https://a/1.jpg"), poster_hash(&text, "https://a/2.jpg"), "a new poster is a new description");
}

#[test]
fn bare_vectors_are_normalised_and_inconsistent_ones_dropped() {
    let store = VectorStore::from_vectors([
        ("radarr-1".to_string(), vec![3.0, 4.0]),
        ("radarr-2".to_string(), vec![0.0, 0.0]),
        ("radarr-3".to_string(), vec![1.0, f32::INFINITY]),
        ("radarr-4".to_string(), vec![1.0, 0.0, 0.0]),
        ("radarr-5".to_string(), vec![0.0, -2.0]),
    ]);
    assert_eq!(store.len(), 2);
    assert_eq!(store.vector("radarr-1"), Some(&[0.6, 0.8][..]));
    assert_eq!(store.vector("radarr-5"), Some(&[0.0, -1.0][..]));
    assert_eq!(store.dimensions(), 2);
}

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("flinch-embedding-{name}-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn filled() -> VectorStore {
    let mut store = VectorStore::default();
    store.retarget("embeddinggemma-2", 2, RECIPE_VERSION);
    store.insert("radarr-1".into(), "aaaa".into(), vec![1.0, 1.0]).expect("a 2-d vector");
    store
}

#[rstest]
#[case::another_model("nomic-embed-text", 2, RECIPE_VERSION)]
#[case::another_truncation("embeddinggemma-2", 4, RECIPE_VERSION)]
#[case::posters_switched_on("embeddinggemma-2", 2, POSTER_RECIPE_VERSION)]
fn another_model_truncation_or_recipe_empties_the_store(#[case] model: &str, #[case] dimensions: u32, #[case] recipe: u32) {
    let mut store = filled();
    assert!(!store.retarget("embeddinggemma-2", 2, RECIPE_VERSION), "the same target keeps every vector");
    assert!(store.retarget(model, dimensions, recipe));
    assert!(store.is_empty());
}

#[test]
fn another_recipe_empties_the_store() {
    let dir = scratch("recipe");
    let mut file: serde_json::Value = serde_json::to_value(filled()).expect("encodes");
    file["recipe"] = serde_json::json!(RECIPE_VERSION + 1);
    std::fs::write(dir.join(STORE_FILE), file.to_string()).expect("write");
    let mut store = VectorStore::read(&dir).expect("reads");
    assert_eq!(store.len(), 1, "read as it is: with embedding off, old vectors keep serving");
    assert!(store.retarget("embeddinggemma-2", 2, RECIPE_VERSION));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn only_a_changed_text_is_stale_and_the_store_survives_a_round_trip() {
    let dir = scratch("roundtrip");
    filled().write(&dir).expect("written");
    let store = VectorStore::read(&dir).expect("read back");
    assert!(store.is_current("radarr-1", "aaaa"));
    assert!(!store.is_current("radarr-1", "bbbb"), "a new description is embedded again");
    assert!(!store.is_current("radarr-2", "aaaa"));
    let vector = store.vector("radarr-1").expect("kept");
    assert!((vector.iter().map(|v| v * v).sum::<f32>() - 1.0).abs() < 1e-6);
    assert!(VectorStore::read(&dir.join("absent")).expect("a missing file").is_empty());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_vector_of_another_length_is_refused() {
    let mut store = filled();
    assert!(matches!(store.insert("radarr-2".into(), "x".into(), vec![1.0, 0.0, 0.0]), Err(StoreError::Unusable { .. })));
    assert_eq!(store.len(), 1);
}

#[rstest]
#[case::same_day(86_400 * 10 + 5, 300)]
#[case::next_day(86_400 * 11, 500)]
fn the_daily_budget_resets_at_utc_midnight(#[case] now: u64, #[case] left: u32) {
    let mut store = VectorStore::default();
    store.spend(200, 86_400 * 10 + 1);
    assert_eq!(store.budget_left(500, now), left);
}
