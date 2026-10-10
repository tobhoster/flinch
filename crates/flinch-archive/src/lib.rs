//! The archive reflex: decide which seasons and movies leave when a library
//! volume is forecast to outgrow its target, at the least expected regret.
//! [`capacity`] forecasts each volume a window ahead; [`plan`] covers each
//! byte target with a 0-1 knapsack over [`score::regret`].
//!
//! Invariants:
//! - Pinned items (favorites, keep lists) and items without watch evidence or
//!   a Plex id are never selected; never-played items only when the operator
//!   enables it and the evidence is complete.
//! - A plan never orphans part of a show: unplayed seasons leave from the
//!   end, played ones from the start.
//! - Nothing is deleted while the planner's `dry_run` is on. Off, Maintainerr
//!   deletes on its own schedule, or the opt-in [`executor`] deletes itself.

pub mod archive;
pub mod arr;
pub mod body;
pub mod capacity;
pub mod card;
pub mod daemon;
pub use daemon::{reconcile, ItemSnapshot, ReconcileOutput, StatusSnapshot};
pub mod dupes;
pub mod embedding;
pub mod executor;
pub mod fit;
pub mod golden;
pub mod govern;
pub mod ids;
pub mod inflow;
pub mod jellyfin;
pub mod maintainerr;
pub mod notify;
pub mod outside;
pub mod overlay;
pub mod persist;
pub mod plan;
pub mod plex;
pub mod presence;
pub mod quality;
pub mod regret;
pub mod requests;
pub mod rules;
pub mod signals;
pub mod taste;
pub mod tautulli;
pub mod themes;
pub mod torrents;
pub mod trash;
pub mod viewers;
pub mod watch;
pub mod watch_sources;

pub use card::{ArchiveCard, LibraryKind};
pub use maintainerr::{HttpMaintainerr, MaintainerrApi, MaintainerrError, MaintainerrTarget};
pub use plan::{generate_eviction_plan, EvictionPlan, MediaCandidate, PlanItem};
