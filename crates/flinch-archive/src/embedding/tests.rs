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
fn the_text_names_the_title_and_its_content_without_any_prompt() {
    let text = movie_text(&matrix(), Some(&matrix_plex()));
    assert!(text.starts_with("Title: The Matrix\nYear: 1999\nKind: movie\n"), "{text}");
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

#[rstest]
#[case::embeddinggemma("embeddinggemma", "task: classification | query: ", "task: search result | query: ")]
#[case::tagged_gemma("embeddinggemma:300m", "task: classification | query: ", "task: search result | query: ")]
#[case::namespaced_gemma("hf.co/google/EmbeddingGemma-300m-GGUF", "task: classification | query: ", "task: search result | query: ")]
#[case::bge("bge-m3", "", "Represent this sentence for searching relevant passages: ")]
#[case::minilm("all-minilm", "", "")]
#[case::nomic("nomic-embed-text", "", "")]
fn each_model_family_keeps_its_own_prompts(#[case] model: &str, #[case] description: &str, #[case] search: &str) {
    assert_eq!((text_prefix(model), query_prompt(model)), (description, search));
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
#[case::with_year("Heat", Some(1995), "Heat (1995)")]
#[case::unknown_year("  Heat\n", None, "Heat")]
#[case::zero_year("Heat", Some(0), "Heat")]
fn a_title_is_named_with_its_year(#[case] title: &str, #[case] year: Option<u32>, #[case] named: &str) {
    assert_eq!(display_title(title, year), named);
}

#[rstest]
#[case::season("sonarr-7-s2", "sonarr-7")]
#[case::specials("sonarr-7-s0", "sonarr-7")]
#[case::movie("radarr-3", "radarr-3")]
#[case::show("sonarr-7", "sonarr-7")]
#[case::named_season("sonarr@anime-7-s2", "sonarr@anime-7")]
#[case::named_movie("radarr@4k-3", "radarr@4k-3")]
#[case::name_starting_with_s("sonarr@sd-7-s1", "sonarr@sd-7")]
fn seasons_share_their_shows_subject(#[case] card: &str, #[case] subject: &str) {
    assert_eq!(subject_of(card), subject);
}

#[rstest]
#[case::truncated_then_normalised(&[3.0, 4.0, 100.0], 2, Some(vec![0.6, 0.8]))]
#[case::exact_length(&[0.0, 2.0], 2, Some(vec![0.0, 1.0]))]
#[case::native_length(&[0.0, 3.0, 4.0], 0, Some(vec![0.0, 0.6, 0.8]))]
#[case::too_short(&[1.0], 2, None)]
#[case::nan_in_the_prefix(&[1.0, f32::NAN, 0.0], 2, None)]
#[case::zero_prefix(&[0.0, 0.0, 1.0], 2, None)]
fn a_matryoshka_prefix_is_renormalised_or_refused(#[case] raw: &[f32], #[case] dimensions: usize, #[case] wanted: Option<Vec<f32>>) {
    assert_eq!(truncate(raw, dimensions), wanted);
}

fn config(model: &str, dimensions: u32) -> EmbeddingConfig {
    EmbeddingConfig { model: model.to_string(), dimensions, ..EmbeddingConfig::default() }
}

#[rstest]
#[case::default(EmbeddingConfig::default(), true)]
#[case::native_length(config("nomic-embed-text", 0), true)]
#[case::matryoshka_size(config("embeddinggemma", 512), true)]
#[case::too_short(config("embeddinggemma", 16), false)]
#[case::too_long(config("embeddinggemma", 8192), false)]
#[case::blank_model(config(" ", 256), false)]
#[case::model_with_spaces(config("embedding gemma", 256), false)]
#[case::no_budget(EmbeddingConfig { daily_budget: 0, ..EmbeddingConfig::default() }, false)]
fn only_an_ollama_model_name_and_a_usable_length_are_accepted(#[case] config: EmbeddingConfig, #[case] valid: bool) {
    assert_eq!(config.validate().is_ok(), valid, "{:?}", config.validate());
}

#[test]
fn a_store_names_its_ollama_model() {
    assert_eq!(model_id(" nomic-embed-text "), "ollama:nomic-embed-text");
    assert_eq!(ollama_model("ollama:all-minilm"), Some("all-minilm"));
    assert_eq!(ollama_model("google/embeddinggemma-2@00000000"), None, "an in-process model's vectors are another space");
    assert_eq!(ollama_model("ollama:"), None);
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

fn heat() -> Description {
    Description { hash: "aaaa".into(), title: "Heat (1995)".into(), text: "Title: Heat".into() }
}

fn filled() -> VectorStore {
    let mut store = VectorStore::default();
    store.retarget("ollama:embeddinggemma", 2, RECIPE_VERSION);
    store.insert("radarr-1".into(), heat(), vec![1.0, 1.0]).expect("a 2-d vector");
    store
}

#[rstest]
#[case::another_model("ollama:nomic-embed-text", 2, RECIPE_VERSION)]
#[case::another_truncation("ollama:embeddinggemma", 4, RECIPE_VERSION)]
#[case::native_length("ollama:embeddinggemma", 0, RECIPE_VERSION)]
#[case::another_recipe("ollama:embeddinggemma", 2, RECIPE_VERSION + 1)]
fn another_model_truncation_or_recipe_empties_the_store(#[case] model: &str, #[case] truncation: u32, #[case] recipe: u32) {
    let mut store = filled();
    assert!(!store.retarget("ollama:embeddinggemma", 2, RECIPE_VERSION), "the same target keeps every vector");
    assert!(store.retarget(model, truncation, recipe));
    assert!(store.is_empty());
}

#[test]
fn the_models_own_length_is_set_by_the_first_vector() {
    let mut store = VectorStore::default();
    store.retarget("ollama:all-minilm", 0, RECIPE_VERSION);
    store.insert("radarr-1".into(), heat(), vec![0.0, 3.0, 4.0]).expect("the first vector sets the length");
    assert_eq!(store.dimensions(), 3);
    assert!(store.insert("radarr-2".into(), heat(), vec![1.0, 0.0]).is_err(), "every later one keeps it");
    assert!(!store.retarget("ollama:all-minilm", 0, RECIPE_VERSION), "the learnt length is not a new target");
}

#[test]
fn another_recipe_empties_the_store() {
    let dir = scratch("recipe");
    let mut file: serde_json::Value = serde_json::to_value(filled()).expect("encodes");
    file["recipe"] = serde_json::json!(RECIPE_VERSION + 1);
    std::fs::write(dir.join(STORE_FILE), file.to_string()).expect("write");
    let mut store = VectorStore::read(&dir).expect("reads");
    assert_eq!(store.len(), 1, "read as it is: with embedding off, old vectors keep serving");
    assert!(store.retarget("ollama:embeddinggemma", 2, RECIPE_VERSION));
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
    assert_eq!((store.title("radarr-1"), store.text("radarr-1")), (Some("Heat (1995)"), Some("Title: Heat")));
    let vector = store.vector("radarr-1").expect("kept");
    assert!((vector.iter().map(|v| v * v).sum::<f32>() - 1.0).abs() < 1e-6);
    assert!(VectorStore::read(&dir.join("absent")).expect("a missing file").is_empty());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn an_older_file_gets_its_titles_and_texts_without_embedding_again() {
    let dir = scratch("describe");
    let mut file: serde_json::Value = serde_json::to_value(filled()).expect("encodes");
    let entry = file["subjects"]["radarr-1"].as_object_mut().expect("an entry");
    entry.remove("title");
    entry.remove("text");
    std::fs::write(dir.join(STORE_FILE), file.to_string()).expect("write");
    let mut store = VectorStore::read(&dir).expect("an older file reads");
    assert_eq!((store.title("radarr-1"), store.text("radarr-1")), (None, None));
    let before = store.vector("radarr-1").map(<[f32]>::to_vec);
    assert!(store.describe("radarr-1", &heat()), "filled in");
    assert!(!store.describe("radarr-1", &heat()), "already current");
    assert!(!store.describe("radarr-1", &Description { hash: "bbbb".into(), ..heat() }), "a new text needs a new vector");
    assert_eq!((store.title("radarr-1"), store.text("radarr-1")), (Some("Heat (1995)"), Some("Title: Heat")));
    assert_eq!(store.vector("radarr-1").map(<[f32]>::to_vec), before, "the vector stays as it was");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_vector_of_another_length_is_refused() {
    let mut store = filled();
    assert!(matches!(store.insert("radarr-2".into(), heat(), vec![1.0, 0.0, 0.0]), Err(StoreError::Unusable { .. })));
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
