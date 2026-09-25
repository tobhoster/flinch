//! Where the daemon's Maintainerr writes go: the live API, or a dry run that
//! prints what it would have done. Both read the live state, so a dry run
//! plans exactly what an enforcing run would.

use flinch_archive::maintainerr::{
    CollectionInfo, ExclusionRow, HttpMaintainerr, MaintainerrApi, MaintainerrError, MaintainerrTarget, MaintainerrVersion,
};

pub(super) enum Sink {
    Live(HttpMaintainerr),
    /// Observation only: the homelab doctrine for a new reflex is to display
    /// and collect for at least one rule cycle before it is trusted. The
    /// daemon uses this whenever enforcement is off.
    DryRun(HttpMaintainerr),
}

impl Sink {
    fn reader(&mut self) -> &mut HttpMaintainerr {
        match self {
            Sink::Live(api) | Sink::DryRun(api) => api,
        }
    }
}

impl MaintainerrApi for Sink {
    fn simulated(&self) -> bool {
        matches!(self, Sink::DryRun(_))
    }

    async fn version(&mut self) -> Result<MaintainerrVersion, MaintainerrError> {
        self.reader().version().await
    }

    async fn collections(&mut self) -> Result<Vec<CollectionInfo>, MaintainerrError> {
        self.reader().collections().await
    }

    async fn collection_members(&mut self, collection_id: i64) -> Result<Vec<String>, MaintainerrError> {
        self.reader().collection_members(collection_id).await
    }

    async fn exclusions(&mut self, media_id: &str) -> Result<Vec<ExclusionRow>, MaintainerrError> {
        self.reader().exclusions(media_id).await
    }

    async fn add_exclusion(&mut self, target: &MaintainerrTarget) -> Result<(), MaintainerrError> {
        match self {
            Sink::Live(api) => api.add_exclusion(target).await,
            Sink::DryRun(_) => {
                println!("[dry-run] would exclude ratingKey {} (mediaId {})", target.item_key(), target.media_id());
                Ok(())
            }
        }
    }

    async fn remove_exclusion(&mut self, exclusion_id: i64) -> Result<(), MaintainerrError> {
        match self {
            Sink::Live(api) => api.remove_exclusion(exclusion_id).await,
            Sink::DryRun(_) => {
                println!("[dry-run] would remove FLINCH exclusion row {exclusion_id}");
                Ok(())
            }
        }
    }

    async fn add_to_collection(&mut self, collection_id: i64, target: &MaintainerrTarget) -> Result<(), MaintainerrError> {
        match self {
            Sink::Live(api) => api.add_to_collection(collection_id, target).await,
            Sink::DryRun(_) => {
                println!(
                    "[dry-run] would add ratingKey {} (mediaId {}) to collection {collection_id}",
                    target.item_key(),
                    target.media_id()
                );
                Ok(())
            }
        }
    }

    async fn remove_from_collection(&mut self, collection_id: i64, item_key: &str) -> Result<(), MaintainerrError> {
        match self {
            Sink::Live(api) => api.remove_from_collection(collection_id, item_key).await,
            Sink::DryRun(_) => {
                println!("[dry-run] would remove ratingKey {item_key} from collection {collection_id}");
                Ok(())
            }
        }
    }
}
