//! Keeping the taste vectors current: once a cycle, the movies and shows whose
//! description has no vector yet are embedded by the operator's Ollama
//! (`settings.json` `ollama`, `embedding.model`), on-disk titles first, within
//! the operator's daily budget and a per-cycle time budget. Switching model or
//! dimensions embeds every title again.
//!
//! Nothing here fails the cycle. Ollama unreachable, or without the model,
//! leaves the cache as it was and says why in the status; switched off, the
//! cached vectors are still read and served, so turning embedding off never
//! takes taste away from the titles already embedded.

use super::state_dir;
use flinch_archive::arr::{ArrMovie, ArrSeries};
use flinch_archive::embedding::{self, Description, EmbeddingConfig, EmbeddingStatus, VectorStore};
use flinch_archive::ids::PlexIds;
use flinch_archive::ollama::{self, OllamaConfig};
use flinch_archive::plex::PlexMetadata;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// The longest one cycle spends embedding: the plan waits for it. A large
/// library fills in over many cycles.
const CYCLE_BUDGET: Duration = Duration::from_secs(120);
/// Texts per Ollama request.
const BATCH: usize = 16;

/// What the refresh reads: the *arr libraries, and Plex rows joined to them
/// through the GUID resolution only (card id → ratingKey), never by title.
pub(super) struct Library<'a> {
    pub(super) movies: &'a [ArrMovie],
    pub(super) series: &'a [ArrSeries],
    /// Movie and show rows by ratingKey.
    pub(super) plex_content: &'a HashMap<String, PlexMetadata>,
    /// Card id → Plex placement; a season's names its show.
    pub(super) plex_ids: &'a HashMap<String, PlexIds>,
}

pub(super) struct Refresh {
    pub(super) vectors: VectorStore,
    pub(super) status: EmbeddingStatus,
}

/// One movie or show, described.
struct Subject {
    id: String,
    description: Description,
    on_disk: bool,
}

/// Every subject of the library, on-disk titles first (they are what the
/// planner scores), then by id so a budget-limited day is reproducible.
fn subjects(library: &Library<'_>) -> Vec<Subject> {
    let plex_row = |card_id: &str| library.plex_ids.get(card_id).and_then(|ids| library.plex_content.get(&ids.rating_key));
    let movies = library.movies.iter().map(|movie| {
        let id = movie.card_id();
        let text = embedding::movie_text(movie, plex_row(&id));
        let description = Description::new(embedding::display_title(&movie.title, movie.year), text);
        Subject { id, description, on_disk: movie.has_file }
    });
    let shows = library.series.iter().map(|series| {
        // Any resolved season names the show's ratingKey.
        let plex = series.seasons.iter().find_map(|season| plex_row(&series.season_card_id(season.season_number)));
        let on_disk = series.seasons.iter().any(|season| season.statistics.episode_file_count > 0);
        let text = embedding::series_text(series, plex);
        let description = Description::new(embedding::display_title(&series.title, series.year), text);
        Subject { id: series.subject(), description, on_disk }
    });
    let mut all: Vec<Subject> = movies.chain(shows).collect();
    all.sort_by(|a, b| b.on_disk.cmp(&a.on_disk).then_with(|| a.id.cmp(&b.id)));
    all
}

/// Read the cache, embed what is missing or stale within today's budget when
/// embedding is on, and write the cache back.
pub(super) async fn refresh(
    http: &reqwest::Client,
    ollama: &OllamaConfig,
    config: &EmbeddingConfig,
    library: Library<'_>,
    now: u64,
) -> Refresh {
    let dir = state_dir();
    let mut problem = None;
    let mut store = VectorStore::read(&dir).unwrap_or_else(|error| {
        problem = Some(format!("{error}; the cache starts over"));
        VectorStore::default()
    });
    let subjects = subjects(&library);
    let model = embedding::model_id(&config.model);
    let mut embedded = 0usize;
    if config.enabled {
        let had = store.len();
        let mut changed = store.retarget(&model, config.dimensions, embedding::RECIPE_VERSION);
        if changed && had > 0 {
            println!("[flinch-arrd] embeddings: model, dimensions or recipe changed; every title is embedded again");
        }
        // A vector still current keeps the title and text it describes
        // (files written before they were kept get them here, unembedded).
        for subject in &subjects {
            changed |= store.describe(&subject.id, &subject.description);
        }
        let budget = store.budget_left(config.daily_budget, now) as usize;
        let pending: Vec<&Subject> =
            subjects.iter().filter(|subject| !store.is_current(&subject.id, &subject.description.hash)).take(budget).collect();
        if !pending.is_empty() {
            let (vectors, failure) = encode(http, ollama, config, &pending).await;
            for (subject, vector) in vectors {
                match store.insert(subject.id.clone(), subject.description.clone(), vector) {
                    Ok(()) => embedded += 1,
                    Err(error) => problem = Some(error.to_string()),
                }
            }
            problem = failure.or(problem);
        }
        store.spend(embedded as u32, now);
        changed |= embedded > 0;
        if changed {
            if let Err(error) = store.write(&dir) {
                problem = Some(format!("{} write failed: {error}", embedding::STORE_FILE));
            }
        }
    }
    let status = EmbeddingStatus {
        configured: config.enabled,
        model: if config.enabled { model } else { store.model().to_string() },
        dimensions: if config.enabled && config.dimensions > 0 { config.dimensions } else { store.dimensions() },
        subjects: subjects.len(),
        with_vector: subjects.iter().filter(|subject| store.vector(&subject.id).is_some()).count(),
        pending: subjects.iter().filter(|subject| !store.is_current(&subject.id, &subject.description.hash)).count(),
        embedded,
        budget_left: if config.enabled { store.budget_left(config.daily_budget, now) } else { 0 },
        problem,
    };
    println!(
        "[flinch-arrd] embeddings{}: {} of {} titles have a vector · {} embedded this cycle · {} still to embed",
        if status.configured { "" } else { " (off: cached vectors only)" },
        status.with_vector,
        status.subjects,
        status.embedded,
        status.pending,
    );
    if let Some(problem) = &status.problem {
        eprintln!("[flinch-arrd] embeddings: {problem}");
    }
    Refresh { vectors: store, status }
}

/// Embed `pending` in order, a batch per request, until it is done or
/// [`CYCLE_BUDGET`] runs out. Returns what was embedded, cut to the configured
/// dimensions, and why it stopped early, if it did.
async fn encode<'s>(
    http: &reqwest::Client,
    ollama: &OllamaConfig,
    config: &EmbeddingConfig,
    pending: &[&'s Subject],
) -> (Vec<(&'s Subject, Vec<f32>)>, Option<String>) {
    let started = Instant::now();
    let model = config.model.trim();
    let prefix = embedding::text_prefix(model);
    let mut done = Vec::new();
    for batch in pending.chunks(BATCH) {
        if started.elapsed() >= CYCLE_BUDGET {
            break;
        }
        let texts: Vec<String> = batch.iter().map(|subject| format!("{prefix}{}", subject.description.text)).collect();
        let texts: Vec<&str> = texts.iter().map(String::as_str).collect();
        let vectors = match ollama::embed(http, ollama, model, &texts).await {
            Ok(vectors) => vectors,
            Err(error) => return (done, Some(error.to_string())),
        };
        for (subject, vector) in batch.iter().zip(vectors) {
            let Some(vector) = embedding::truncate(&vector, config.dimensions as usize) else {
                let problem = format!(
                    "{model} gave {} a {}-d vector that cannot be cut to {} dimensions: lower the dimensions or choose 0 (the model's own)",
                    subject.id,
                    vector.len(),
                    config.dimensions
                );
                return (done, Some(problem));
            };
            done.push((*subject, vector));
        }
    }
    (done, None)
}
