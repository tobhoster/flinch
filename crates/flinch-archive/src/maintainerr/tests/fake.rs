//! An in-memory Maintainerr with the semantics that matter to the sync
//! (checked against v3.29): a season exclusion writes the season's row and
//! its episodes' rows; an exclusion for a key that already has a global row
//! reuses that row; `?mediaServerId=` returns the key's rows and its
//! children's; a collection refuses an item of the wrong kind. Failures are
//! scripted per operation.

use super::super::{CollectionInfo, ExclusionRow, MaintainerrApi, MaintainerrError, MaintainerrTarget, MaintainerrVersion};
use crate::card::LibraryKind;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Op {
    AddExclusion,
    RemoveExclusion,
    AddToCollection,
    RemoveFromCollection,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum Fault {
    /// A non-2xx answer, e.g. 409 while Maintainerr holds its lock.
    Status(u16),
    /// A 2xx whose body says `code: 0`.
    Refused,
    /// Accepted, but nothing changes: only a read-back can tell.
    Lie,
}

#[derive(Debug, Clone)]
pub(crate) struct Fake {
    pub version: MaintainerrVersion,
    pub collections: Vec<CollectionInfo>,
    pub members: BTreeMap<i64, BTreeSet<String>>,
    pub rows: Vec<ExclusionRow>,
    pub next_id: i64,
    pub faults: BTreeMap<Op, VecDeque<Fault>>,
    /// Behave like the dry-run sink: writes succeed and change nothing.
    pub simulated: bool,
    pub writes: Vec<(Op, String)>,
}

impl Fake {
    pub fn new(version: MaintainerrVersion, collections: Vec<CollectionInfo>) -> Self {
        let members = collections.iter().map(|c| (c.id, BTreeSet::new())).collect();
        Self {
            version,
            collections,
            members,
            rows: Vec::new(),
            next_id: 1000,
            faults: BTreeMap::new(),
            simulated: false,
            writes: Vec::new(),
        }
    }

    pub fn fail(&mut self, op: Op, fault: Fault) {
        self.faults.entry(op).or_default().push_back(fault);
    }

    /// Record the write; `true` means apply it.
    fn write(&mut self, op: Op, what: String) -> Result<bool, MaintainerrError> {
        self.writes.push((op, what));
        if self.simulated {
            return Ok(false);
        }
        match self.faults.get_mut(&op).and_then(VecDeque::pop_front) {
            None => Ok(true),
            Some(Fault::Lie) => Ok(false),
            Some(Fault::Status(status)) => Err(MaintainerrError::Http { endpoint: "fake", status, message: "scripted".to_string() }),
            Some(Fault::Refused) => {
                Err(MaintainerrError::Refused { endpoint: "fake", code: 0, message: "Failed - no metadata".to_string() })
            }
        }
    }

    fn upsert(&mut self, media_server_id: &str, parent: &str, media_type: &str) {
        let existing = self.rows.iter_mut().find(|r| r.media_server_id == media_server_id && r.rule_group_id.is_none());
        match existing {
            Some(row) => row.parent = Some(parent.to_string()),
            None => {
                self.next_id += 1;
                self.rows.push(ExclusionRow {
                    id: self.next_id,
                    media_server_id: media_server_id.to_string(),
                    rule_group_id: None,
                    parent: Some(parent.to_string()),
                    media_type: Some(media_type.to_string()),
                });
            }
        }
    }
}

impl MaintainerrApi for Fake {
    fn simulated(&self) -> bool {
        self.simulated
    }

    async fn version(&mut self) -> Result<MaintainerrVersion, MaintainerrError> {
        Ok(self.version.clone())
    }

    async fn collections(&mut self) -> Result<Vec<CollectionInfo>, MaintainerrError> {
        Ok(self.collections.clone())
    }

    async fn collection_members(&mut self, collection_id: i64) -> Result<Vec<String>, MaintainerrError> {
        Ok(self.members.get(&collection_id).map(|m| m.iter().cloned().collect()).unwrap_or_default())
    }

    async fn exclusions(&mut self, media_id: &str) -> Result<Vec<ExclusionRow>, MaintainerrError> {
        Ok(self.rows.iter().filter(|r| r.media_server_id == media_id || r.parent.as_deref() == Some(media_id)).cloned().collect())
    }

    async fn add_exclusion(&mut self, target: &MaintainerrTarget) -> Result<(), MaintainerrError> {
        if !self.write(Op::AddExclusion, target.item_key().to_string())? {
            return Ok(());
        }
        match target {
            MaintainerrTarget::Movie { rating_key } => self.upsert(rating_key, rating_key, "movie"),
            MaintainerrTarget::Season { show_rating_key, season_rating_key } => {
                self.upsert(season_rating_key, show_rating_key, "season");
                self.upsert(&format!("{season_rating_key}-e1"), show_rating_key, "episode");
            }
        }
        Ok(())
    }

    async fn remove_exclusion(&mut self, exclusion_id: i64) -> Result<(), MaintainerrError> {
        if self.write(Op::RemoveExclusion, exclusion_id.to_string())? {
            self.rows.retain(|r| r.id != exclusion_id);
        }
        Ok(())
    }

    async fn add_to_collection(&mut self, collection_id: i64, target: &MaintainerrTarget) -> Result<(), MaintainerrError> {
        let Some(collection) = self.collections.iter().find(|c| c.id == collection_id) else {
            return Err(MaintainerrError::Http { endpoint: "fake", status: 404, message: "not found".to_string() });
        };
        let fits = match target.kind() {
            LibraryKind::Movie => collection.media_type == "movie",
            LibraryKind::Season => collection.media_type == "season",
        };
        if !fits {
            let message = "This item cannot be applied to the selected collection".to_string();
            return Err(MaintainerrError::Http { endpoint: "fake", status: 400, message });
        }
        if self.write(Op::AddToCollection, target.item_key().to_string())? {
            self.members.entry(collection_id).or_default().insert(target.item_key().to_string());
        }
        Ok(())
    }

    async fn remove_from_collection(&mut self, collection_id: i64, item_key: &str) -> Result<(), MaintainerrError> {
        if self.write(Op::RemoveFromCollection, item_key.to_string())? {
            if let Some(members) = self.members.get_mut(&collection_id) {
                members.remove(item_key);
            }
        }
        Ok(())
    }
}
