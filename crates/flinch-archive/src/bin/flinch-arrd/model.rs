//! Which hazard a cycle runs: the daily refit, then the adopted fit or the
//! hand-set priors. The choice is always reported: a model that changes
//! without saying so is indistinguishable from a bug when the numbers move.

use flinch_archive::fit::{self, adopt};
use flinch_archive::regret::HazardModel;
use flinch_archive::taste::Record;
use std::path::Path;

/// The hazard a cycle runs.
pub(super) struct Running {
    pub(super) hazard: HazardModel,
    /// The outcomes the adopted fit's taste was learned from; `None` under the
    /// priors, whose taste weight is zero.
    pub(super) outcomes: Option<Record>,
    pub(super) label: String,
}

/// Refit if a day has passed, then the hazard to run and its description.
pub(super) fn hazard(state_dir: &Path, now: u64) -> Running {
    match adopt::refit_if_due(state_dir, now) {
        None => {}
        Some(Ok(status)) => match &status.shortfall {
            None => println!("[flinch-arrd] fit: adopted the {} ({})", status.kind.label(), status.fitted_on),
            Some(shortfall) => println!("[flinch-arrd] fit: priors kept — {shortfall} ({})", status.fitted_on),
        },
        Some(Err(error)) => eprintln!("[flinch-arrd] fit failed, priors kept: {error}"),
    }
    let running = match (fit::load_model(state_dir), adopt::read_status(state_dir)) {
        (Some(model), Some(status)) => Running {
            hazard: model.hazard,
            outcomes: Some(model.outcomes),
            label: format!("{} — out-of-fold AUC {:.2}, Brier {:.3}", status.kind.label(), status.metrics.auc, status.metrics.brier),
        },
        (Some(model), None) => Running { hazard: model.hazard, outcomes: Some(model.outcomes), label: "fitted hazard".to_string() },
        (None, _) => {
            Running { hazard: HazardModel::default(), outcomes: None, label: "priors (no fit has beaten them out of fold yet)".to_string() }
        }
    };
    println!("[flinch-arrd] model: {}", running.label);
    running
}
