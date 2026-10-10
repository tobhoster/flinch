//! The vector cache: `embeddings.json` in the state directory.
//!
//! One file holds the model, dimensions and recipe version every vector was
//! made with, and per subject the hash of the text it was made from. Vectors
//! from another model, truncation or recipe live in another space, so a store
//! retargeted to a new one starts empty; a subject whose text hash changed is
//! re-embedded on its own. The day's embedding count travels in the same file,
//! so a restart does not reset the daily budget.
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
    #[serde(default)]
    dimensions: u32,
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
    vector: Vec<f32>,
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
    /// length differs from the first one kept. Model, recipe and text hashes
    /// are left blank, so a daemon retargeting it would start over.
    pub fn from_vectors(vectors: impl IntoIterator<Item = (String, Vec<f32>)>) -> Self {
        let (subjects, length) =
            consistent(vectors.into_iter().map(|(subject, vector)| (subject, Entry { hash: String::new(), vector })), None);
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

    /// The model every vector came from; empty when unknown.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Point the store at `model` truncated to `dimensions` under `recipe`
    /// ([`super::RECIPE_VERSION`], or [`super::POSTER_RECIPE_VERSION`] with
    /// posters). Any change empties it: vectors of another space cannot be
    /// compared with the new ones. Returns whether it was emptied of
    /// anything. The day's budget use stands.
    pub fn retarget(&mut self, model: &str, dimensions: u32, recipe: u32) -> bool {
        if self.model == model && self.dimensions == dimensions && self.recipe == recipe {
            return false;
        }
        let dropped = !self.subjects.is_empty();
        self.subjects.clear();
        (self.model, self.dimensions, self.recipe) = (model.to_string(), dimensions, recipe);
        dropped
    }

    /// Whether `subject` has a vector made from the text hashing to `hash`.
    pub fn is_current(&self, subject: &str, hash: &str) -> bool {
        self.subjects.get(subject).is_some_and(|entry| entry.hash == hash)
    }

    /// Store `subject`'s vector for the text hashing to `hash`, normalised.
    pub fn insert(&mut self, subject: String, hash: String, vector: Vec<f32>) -> Result<(), StoreError> {
        match unit(vector) {
            Some(vector) if vector.len() == self.dimensions as usize => {
                self.subjects.insert(subject, Entry { hash, vector });
                Ok(())
            }
            _ => Err(StoreError::Unusable { subject, dimensions: self.dimensions }),
        }
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
            kept.insert(subject, Entry { hash: entry.hash, vector });
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
