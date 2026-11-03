//! The vector cache: `embeddings.json` in the state directory.
//!
//! One file holds the model, truncation and recipe version every vector was
//! made with, and per subject the hash of the text it was made from, that
//! text, and the title it names (the taste judge reads both, and the offline
//! fit has only the state directory). Vectors from another model, truncation
//! or recipe live in another space, so a store retargeted to a new one starts
//! empty; a subject whose text hash changed is re-embedded on its own. The
//! day's embedding count travels in the same file, so a restart does not
//! reset the daily budget.
//!
//! Invariant: every stored vector is finite, L2-normalised and of one length.

use super::STORE_FILE;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

const DAY_SECS: u64 = 86_400;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("{STORE_FILE} unreadable: {0}")]
    Io(#[from] std::io::Error),
    #[error("{STORE_FILE} malformed: {0}")]
    Json(#[from] serde_json::Error),
    /// A vector that is zero, not finite, or of another length than the
    /// store's: refused rather than breaking the one-length invariant.
    #[error("the vector for {subject} is unusable: zero, not finite, or not {dimensions}-d")]
    Unusable { subject: String, dimensions: u32 },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VectorStore {
    #[serde(default)]
    model: String,
    /// The length of every vector; 0 while none is known.
    #[serde(default)]
    dimensions: u32,
    /// The truncation the vectors were cut to; 0 for the model's own length.
    #[serde(default)]
    truncation: u32,
    #[serde(default)]
    recipe: u32,
    #[serde(default)]
    budget: Budget,
    #[serde(default)]
    subjects: BTreeMap<String, Entry>,
}

/// Subjects embedded on one UTC day (days since the epoch).
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
struct Budget {
    day: u64,
    used: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Entry {
    /// [`super::text_hash`] of the text embedded; empty when unknown.
    hash: String,
    /// [`super::display_title`]; empty in files written before it was kept.
    #[serde(default)]
    title: String,
    /// The description embedded ([`super::movie_text`]); empty in files
    /// written before it was kept.
    #[serde(default)]
    text: String,
    vector: Vec<f32>,
}

/// What a subject's vector describes: the text, its hash, and the title.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Description {
    pub hash: String,
    pub title: String,
    pub text: String,
}

impl Description {
    /// The description of `text`, hashed, naming `title`.
    pub fn new(title: String, text: String) -> Self {
        Self { hash: super::text_hash(&text), title, text }
    }
}

impl VectorStore {
    /// The cache in `state_dir`; a missing file is an empty store. Entries
    /// breaking the invariant (zero, non-finite, another length) are dropped.
    pub fn read(state_dir: &Path) -> Result<Self, StoreError> {
        let text = match std::fs::read_to_string(state_dir.join(STORE_FILE)) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(error) => return Err(error.into()),
        };
        let mut store: Self = serde_json::from_str(&text)?;
        let declared = (store.dimensions > 0).then_some(store.dimensions as usize);
        let (subjects, length) = consistent(std::mem::take(&mut store.subjects), declared);
        store.subjects = subjects;
        store.dimensions = length.map_or(store.dimensions, |length| length as u32);
        Ok(store)
    }

    /// A store from bare vectors, for tests and offline fits. Each vector is
    /// L2-normalised; zero and non-finite ones are dropped, and so is any whose
    /// length differs from the first one kept. Model, recipe, text hashes,
    /// titles and texts are left blank, so a daemon retargeting it would start
    /// over.
    pub fn from_vectors(vectors: impl IntoIterator<Item = (String, Vec<f32>)>) -> Self {
        let blank = |vector| Entry { hash: String::new(), title: String::new(), text: String::new(), vector };
        let (subjects, length) = consistent(vectors.into_iter().map(|(subject, vector)| (subject, blank(vector))), None);
        Self { dimensions: length.unwrap_or(0) as u32, subjects, ..Self::default() }
    }

    pub fn write(&self, state_dir: &Path) -> Result<(), StoreError> {
        crate::persist::replace(&state_dir.join(STORE_FILE), &serde_json::to_vec(self)?)?;
        Ok(())
    }

    /// The subject's unit vector (see [`super::subject_of`]).
    pub fn vector(&self, subject: &str) -> Option<&[f32]> {
        self.subjects.get(subject).map(|entry| entry.vector.as_slice())
    }

    /// The title the subject's vector names; `None` when not kept.
    pub fn title(&self, subject: &str) -> Option<&str> {
        self.subjects.get(subject).map(|entry| entry.title.as_str()).filter(|title| !title.is_empty())
    }

    /// The description the subject's vector was made from; `None` when not kept.
    pub fn text(&self, subject: &str) -> Option<&str> {
        self.subjects.get(subject).map(|entry| entry.text.as_str()).filter(|text| !text.is_empty())
    }

    pub fn len(&self) -> usize {
        self.subjects.len()
    }

    pub fn is_empty(&self) -> bool {
        self.subjects.is_empty()
    }

    /// The length of every vector; 0 for an empty store.
    pub fn dimensions(&self) -> u32 {
        self.dimensions
    }

    /// The model every vector came from ([`super::model_id`]); empty when unknown.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Point the store at `model` cut to `truncation` (0: the model's own
    /// length) under `recipe` ([`super::RECIPE_VERSION`]). Any change empties
    /// it: vectors of another space cannot be compared with the new ones.
    /// Returns whether anything changed. The day's budget use stands.
    pub fn retarget(&mut self, model: &str, truncation: u32, recipe: u32) -> bool {
        if self.model == model && self.truncation == truncation && self.recipe == recipe {
            return false;
        }
        self.subjects.clear();
        (self.model, self.dimensions, self.truncation, self.recipe) = (model.to_string(), truncation, truncation, recipe);
        true
    }

    /// Whether `subject` has a vector made from the text hashing to `hash`.
    pub fn is_current(&self, subject: &str, hash: &str) -> bool {
        self.subjects.get(subject).is_some_and(|entry| entry.hash == hash)
    }

    /// Keep `description`'s title and text with a vector still current for
    /// it, without embedding again (files written before they were kept, or a
    /// title renamed outside the text). Returns whether anything changed.
    pub fn describe(&mut self, subject: &str, description: &Description) -> bool {
        match self.subjects.get_mut(subject) {
            Some(entry) if entry.hash == description.hash && (entry.title != description.title || entry.text != description.text) => {
                entry.title.clone_from(&description.title);
                entry.text.clone_from(&description.text);
                true
            }
            _ => false,
        }
    }

    /// Store `subject`'s vector for `description`, normalised. The first
    /// vector of a store keeping the model's own length sets that length.
    pub fn insert(&mut self, subject: String, description: Description, vector: Vec<f32>) -> Result<(), StoreError> {
        let Some(vector) = unit(vector) else { return Err(StoreError::Unusable { subject, dimensions: self.dimensions }) };
        if self.dimensions == 0 && self.subjects.is_empty() {
            self.dimensions = vector.len() as u32;
        }
        if vector.len() != self.dimensions as usize {
            return Err(StoreError::Unusable { subject, dimensions: self.dimensions });
        }
        let Description { hash, title, text } = description;
        self.subjects.insert(subject, Entry { hash, title, text, vector });
        Ok(())
    }

    /// Subjects that may still be embedded on the UTC day of `now`.
    pub fn budget_left(&self, daily: u32, now: u64) -> u32 {
        if self.budget.day == now / DAY_SECS {
            daily.saturating_sub(self.budget.used)
        } else {
            daily
        }
    }

    /// Count `embedded` subjects against the UTC day of `now`.
    pub fn spend(&mut self, embedded: u32, now: u64) {
        let day = now / DAY_SECS;
        if self.budget.day != day {
            self.budget = Budget { day, used: 0 };
        }
        self.budget.used = self.budget.used.saturating_add(embedded);
    }
}

/// The entries that keep the invariant, and their common length: `length`
/// when given, else the first usable vector's.
fn consistent(entries: impl IntoIterator<Item = (String, Entry)>, mut length: Option<usize>) -> (BTreeMap<String, Entry>, Option<usize>) {
    let mut kept = BTreeMap::new();
    for (subject, entry) in entries {
        let Some(vector) = unit(entry.vector) else { continue };
        if *length.get_or_insert(vector.len()) == vector.len() {
            kept.insert(subject, Entry { vector, ..entry });
        }
    }
    (kept, length)
}

/// `vector` scaled to length 1; `None` when it is empty, zero or not finite.
pub(super) fn unit(mut vector: Vec<f32>) -> Option<Vec<f32>> {
    if vector.iter().any(|value| !value.is_finite()) {
        return None;
    }
    let norm = vector.iter().map(|value| f64::from(*value).powi(2)).sum::<f64>().sqrt();
    if !(norm > 0.0 && norm.is_finite()) {
        return None;
    }
    for value in &mut vector {
        *value = (f64::from(*value) / norm) as f32;
    }
    Some(vector)
}
