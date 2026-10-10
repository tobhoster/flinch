//! Keeping the taste vectors current: once a cycle, the movies and shows whose
//! description has no vector yet are embedded in-process by EmbeddingGemma 2,
//! on-disk titles first, within the operator's daily budget and a per-cycle
//! time budget. With posters on, each description holds the title's poster
//! too (see [`posters`]); those vectors are a space of their own, so turning
//! posters on or off embeds every title again.
//!
//! Nothing here fails the cycle. A download or model error leaves the cache as
//! it was and says why in the status; switched off, the cached vectors are
//! still read and served, so turning embedding off never takes taste away from
//! the titles already embedded.

mod posters;

use super::state_dir;
use flinch_archive::arr::{ArrMovie, ArrSeries};
use flinch_archive::embedding::{self, EmbeddingConfig, EmbeddingStatus, Encoder, ImageTokens, ModelFiles, VectorStore, VisionEncoder};
use flinch_archive::ids::PlexIds;
use flinch_archive::plex::PlexMetadata;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// The longest one cycle spends embedding: the plan waits for it. A large
/// library fills in over many cycles.
const CYCLE_BUDGET: Duration = Duration::from_secs(120);
/// Texts per encoder pass: each layer's weights are read once per batch.
const BATCH: usize = 16;
/// Titles per pass with posters: each poster takes seconds of vision tower
/// first, and the cycle budget is checked between passes.
const POSTER_BATCH: usize = 4;

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
    /// The description without a poster.
    text: String,
    /// The cache key of what is embedded: the text, and the poster URL with posters on.
    hash: String,
    /// The upstream poster, with posters on.
    poster: Option<String>,
    on_disk: bool,
}

/// Every subject of the library, on-disk titles first (they are what the
/// planner scores), then by id so a budget-limited day is reproducible.
fn subjects(library: &Library<'_>, with_posters: bool) -> Vec<Subject> {
    let plex_row = |card_id: &str| library.plex_ids.get(card_id).and_then(|ids| library.plex_content.get(&ids.rating_key));
    let describe = |id: String, text: String, poster: Option<String>, on_disk: bool| {
        let poster = poster.filter(|_| with_posters);
        let hash = match &poster {
            Some(url) => embedding::poster_hash(&embedding::with_poster(&text), url),
            None => embedding::text_hash(&text),
        };
        Subject { id, text, hash, poster, on_disk }
    };
    let movies = library.movies.iter().map(|movie| {
        let id = movie.card_id();
        let text = embedding::movie_text(movie, plex_row(&id));
        describe(id, text, posters::poster_url(&movie.images), movie.has_file)
    });
    let shows = library.series.iter().map(|series| {
        // Any resolved season names the show's ratingKey.
        let plex = series.seasons.iter().find_map(|season| plex_row(&series.season_card_id(season.season_number)));
        let on_disk = series.seasons.iter().any(|season| season.statistics.episode_file_count > 0);
        let text = embedding::series_text(series, plex);
        describe(series.subject(), text, posters::poster_url(&series.images), on_disk)
    });
    let mut all: Vec<Subject> = movies.chain(shows).collect();
    all.sort_by(|a, b| b.on_disk.cmp(&a.on_disk).then_with(|| a.id.cmp(&b.id)));
    all
}

/// One subject to embed, owned: the encoder runs on a blocking thread.
struct Pending {
    id: String,
    hash: String,
    text: String,
    poster: Option<String>,
}

/// Read the cache, embed what is missing or stale within today's budget when
/// embedding is on, and write the cache back.
pub(super) async fn refresh(config: &EmbeddingConfig, library: Library<'_>, now: u64) -> Refresh {
    let dir = state_dir();
    let mut problem = None;
    let mut store = VectorStore::read(&dir).unwrap_or_else(|error| {
        problem = Some(format!("{error}; the cache starts over"));
        VectorStore::default()
    });
    let subjects = subjects(&library, config.posters);
    let (model, recipe) = if config.posters {
        (embedding::poster_model_id(), embedding::POSTER_RECIPE_VERSION)
    } else {
        (embedding::model_id(), embedding::RECIPE_VERSION)
    };
    let mut embedded = 0usize;
    if config.enabled {
        let mut changed = store.retarget(&model, config.dimensions, recipe);
        if changed {
            println!("[flinch-arrd] embeddings: model, dimensions, recipe or posters changed; every title is embedded again");
        }
        let budget = store.budget_left(config.daily_budget, now) as usize;
        let pending: Vec<Pending> = subjects
            .iter()
            .filter(|subject| !store.is_current(&subject.id, &subject.hash))
            .take(budget)
            .map(|subject| Pending {
                id: subject.id.clone(),
                hash: subject.hash.clone(),
                text: subject.text.clone(),
                poster: subject.poster.clone(),
            })
            .collect();
        if !pending.is_empty() {
            let (vectors, failure) = encode(pending, config.dimensions as usize, config.posters).await;
            for (id, hash, vector) in vectors {
                match store.insert(id, hash, vector) {
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
        dimensions: if config.enabled { config.dimensions } else { store.dimensions() },
        subjects: subjects.len(),
        with_vector: subjects.iter().filter(|subject| store.vector(&subject.id).is_some()).count(),
        pending: subjects.iter().filter(|subject| !store.is_current(&subject.id, &subject.hash)).count(),
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

/// Fetch the model on first use, then embed `pending` in order until it is
/// done or [`CYCLE_BUDGET`] runs out. Returns what was embedded, truncated to
/// `dimensions`, and why it stopped early or left titles out, if it did.
async fn encode(pending: Vec<Pending>, dimensions: usize, with_posters: bool) -> (Vec<(String, String, Vec<f32>)>, Option<String>) {
    let files = ModelFiles::in_state(&state_dir());
    let fetched = if with_posters && !files.vision_present() {
        println!("[flinch-arrd] embeddings: downloading the {} text and vision weights (about 580 + 335 MB, once)", embedding::model_id());
        files.fetch_vision().await
    } else if !files.present() {
        println!("[flinch-arrd] embeddings: downloading the {} text weights (about 580 MB, once)", embedding::model_id());
        files.fetch().await
    } else {
        Ok(())
    };
    if let Err(error) = fetched {
        return (Vec::new(), Some(error.to_string()));
    }
    let runtime = tokio::runtime::Handle::current();
    let worker = tokio::task::spawn_blocking(move || {
        let started = Instant::now();
        let mut done = Vec::new();
        let encoder = match Encoder::open(&files) {
            Ok(encoder) => encoder,
            Err(error) => return (done, Some(error.to_string())),
        };
        let vision = match with_posters.then(|| VisionEncoder::open(&files)).transpose() {
            Ok(vision) => vision,
            Err(error) => return (done, Some(error.to_string())),
        };
        let mut posters = match vision.as_ref().map(|vision| posters::Posters::new(vision, runtime)).transpose() {
            Ok(posters) => posters,
            Err(error) => return (done, Some(format!("poster client: {error}"))),
        };
        for batch in pending.chunks(if posters.is_some() { POSTER_BATCH } else { BATCH }) {
            if started.elapsed() >= CYCLE_BUDGET {
                break;
            }
            let embedded = match posters.as_mut() {
                Some(posters) => embed_with_posters(&encoder, posters, batch),
                None => {
                    let texts: Vec<&str> = batch.iter().map(|subject| subject.text.as_str()).collect();
                    encoder.embed(&texts).map(|vectors| batch.iter().zip(vectors).collect())
                }
            };
            let embedded = match embedded {
                Ok(embedded) => embedded,
                Err(error) => return (done, Some(error.to_string())),
            };
            for (subject, vector) in embedded {
                let Some(vector) = embedding::truncate(&vector, dimensions) else {
                    return (done, Some(format!("the model gave {} an unusable vector", subject.id)));
                };
                done.push((subject.id.clone(), subject.hash.clone(), vector));
            }
        }
        let problem = posters.and_then(|posters| posters.problem());
        (done, problem)
    });
    worker.await.unwrap_or_else(|error| (Vec::new(), Some(format!("the embedding worker stopped: {error}"))))
}

/// One batch with posters; a title whose poster must wait for a later cycle
/// is left out of it.
fn embed_with_posters<'p>(
    encoder: &Encoder,
    posters: &mut posters::Posters<'_>,
    batch: &'p [Pending],
) -> Result<Vec<(&'p Pending, Vec<f32>)>, embedding::ModelError> {
    let mut described = Vec::with_capacity(batch.len());
    for subject in batch {
        if let Some((text, image)) = posters.describe(&subject.text, subject.poster.as_deref())? {
            described.push((subject, text, image));
        }
    }
    if described.is_empty() {
        return Ok(Vec::new());
    }
    let inputs: Vec<(&str, Option<&ImageTokens>)> = described.iter().map(|(_, text, image)| (text.as_str(), image.as_ref())).collect();
    let vectors = encoder.embed_with_images(&inputs)?;
    Ok(described.into_iter().map(|(subject, _, _)| subject).zip(vectors).collect())
}
