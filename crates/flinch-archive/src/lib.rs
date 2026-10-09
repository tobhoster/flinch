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
//! - Nothing is deleted here. Hand-offs to Maintainerr happen only when the
//!   planner's `dry_run` is off; Maintainerr deletes on its own schedule.

pub mod arr;
pub mod body;
pub mod capacity;
pub mod card;
pub mod daemon;
pub use daemon::{reconcile, ItemSnapshot, ReconcileOutput, StatusSnapshot};
pub mod embedding;
pub mod fit;
pub mod golden;
pub mod govern;
pub mod ids;
pub mod inflow;
pub mod maintainerr;
pub mod outside;
pub mod persist;
pub mod plan;
pub mod plex;
pub mod presence;
pub mod quality;
pub mod regret;
pub mod signals;
pub mod taste;
pub mod tautulli;
pub mod themes;
pub mod watch;

pub use card::{ArchiveCard, LibraryKind};
pub use maintainerr::{HttpMaintainerr, MaintainerrApi, MaintainerrError, MaintainerrTarget};
pub use plan::{generate_eviction_plan, EvictionPlan, MediaCandidate, PlanItem};
