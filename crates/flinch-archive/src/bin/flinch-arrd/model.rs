//! Which hazard a cycle runs: the daily refit, then the adopted fit or the
//! hand-set priors. The choice is always reported: a model that changes
//! without saying so is indistinguishable from a bug when the numbers move.

use flinch_archive::fit::{self, adopt};
use flinch_archive::regret::HazardModel;
use std::path::Path;

/// Refit if a day has passed, then the hazard to run and its description.
pub(super) fn hazard(state_dir: &Path, now: u64) -> (HazardModel, String) {
    match adopt::refit_if_due(state_dir, now) {
        None => {}
        Some(Ok(status)) => match &status.shortfall {
            None => println!("[flinch-arrd] fit: adopted the {} ({})", status.kind.label(), status.fitted_on),
            Some(shortfall) => println!("[flinch-arrd] fit: priors kept — {shortfall} ({})", status.fitted_on),
        },
        Some(Err(error)) => eprintln!("[flinch-arrd] fit failed, priors kept: {error}"),
    }
    let (model, label) = match (fit::load_model(state_dir), adopt::read_status(state_dir)) {
        (Some(model), Some(status)) => {
            (model, format!("{} — out-of-fold AUC {:.2}, Brier {:.3}", status.kind.label(), status.metrics.auc, status.metrics.brier))
        }
        (Some(model), None) => (model, "fitted hazard".to_string()),
        (None, _) => (HazardModel::default(), "priors (no fit has beaten them out of fold yet)".to_string()),
    };
    println!("[flinch-arrd] model: {label}");
    (model, label)
}
