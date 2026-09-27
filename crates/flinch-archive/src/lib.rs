//! The archive reflex: sharp, calibrated, auditable decisions about which
//! watched seasons and movies can be reclaimed. Deterministic policy first; a
//! fitted model reaches the plan as each card's [`policy::ScoreVerdict`]
//! P(safe), capped at the hand-set priors' ([`score::score_fenced`]), and is
//! judged by calibration and sharpness together (see [`calibration`]).
//!
//! Invariants:
//! - The policy's protections (favorites, keep-collections, active items, the
//!   newest aired season) are never overridden by any model.
//! - Nothing is deleted. This crate produces a plan; a separate explicit
//!   `apply` step moves candidates to trash with a TTL.
//! - A delete requires BOTH the deterministic rule AND, when the card has a
//!   score verdict, its P(safe) at or above the policy's `score_floor`. The
//!   plan's 0.95 `delete_floor` sees only the shipped `Baseline`'s answer, 1.0
//!   for every delete the rule permits (see [`plan`]). Below a floor: keep.

pub mod arr;
pub mod body;
pub mod calibration;
pub mod capacity;
pub mod card;
pub mod daemon;
pub use daemon::{reconcile, ItemSnapshot, ReconcileOutput, StatusSnapshot};
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
pub mod policy;
pub mod presence;
pub mod score;
pub mod shadow;
pub mod systemone;
pub mod taste;
pub mod tautulli;
pub mod watch;

pub use card::{ArchiveCard, LibraryKind, Recency, SeasonState, SeriesType};
pub use maintainerr::{HttpMaintainerr, MaintainerrApi, MaintainerrError, MaintainerrTarget};
pub use plan::{ArchiveModel, Baseline, Plan, PlanEntry, QualityReport};
pub use policy::{is_safe_label, reclaims_bytes, ArchivePolicy, Reason};
