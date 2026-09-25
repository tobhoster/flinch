//! Scoring metrics for the reclaim model.
//!
//! Four numbers decide whether a fitted model replaces the hand-set priors, and
//! they answer different questions: discrimination (AUC), overall accuracy of
//! the probabilities (Brier, log-loss), whether a stated 0.8 means 80% (ECE),
//! and what the operating threshold would actually do (precision/recall at the
//! floor). AUC matters most once storage is over its ceiling: eviction frees the
//! lowest expected regret per byte first, so the *ordering* of P(safe) decides
//! which items go.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Equal-width calibration bins. One constant, so every ECE this crate prints —
/// the adoption gate and the head-to-head alike — is the same quantity.
pub const ECE_BINS: usize = 10;
/// Probabilities are clipped to `[ε, 1 − ε]` for log-loss, so one confidently
/// wrong row costs `−ln ε ≈ 13.8` instead of infinity and the mean stays finite.
pub const LOG_LOSS_EPSILON: f64 = 1e-6;
/// Bootstrap resamples behind every [`Spread`] interval: enough that the 2.5th
/// and 97.5th percentiles are 25 draws from the edge, cheap at panel size.
pub const BOOTSTRAP_RESAMPLES: usize = 1000;
/// Fixed, so the same panel always reports the same interval.
const BOOTSTRAP_SEED: u64 = 0x0f11_1c45_eed0_2026;
/// A forecast of at least this, or at most `1 − CONFIDENT`, is "confident".
pub const CONFIDENT: f32 = 0.9;

/// Area under the ROC curve, computed by rank (ties share the average rank).
///
/// `0.5` is a coin flip, and also the answer when only one class is present:
/// there is nothing to rank. O(n log n), so a large panel is no slower to judge
/// than it is to build.
pub fn auc(scores: &[f32], labels: &[f32]) -> f32 {
    let n = scores.len().min(labels.len());
    let positives = labels[..n].iter().filter(|label| **label >= 0.5).count();
    let negatives = n - positives;
    if positives == 0 || negatives == 0 {
        return 0.5;
    }
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|a, b| scores[*a].total_cmp(&scores[*b]));
    let mut positive_rank_sum = 0.0f64;
    let mut start = 0;
    while start < n {
        let mut end = start;
        while end + 1 < n && scores[order[end + 1]] == scores[order[start]] {
            end += 1;
        }
        // 1-based ranks start+1 ..= end+1, averaged over the tie group.
        let rank = (start + end) as f64 / 2.0 + 1.0;
        positive_rank_sum += rank * order[start..=end].iter().filter(|index| labels[**index] >= 0.5).count() as f64;
        start = end + 1;
    }
    let wins = positive_rank_sum - (positives * (positives + 1)) as f64 / 2.0;
    (wins / (positives * negatives) as f64) as f32
}

/// Mean negative log-likelihood of the labels (natural log), clipped by
/// [`LOG_LOSS_EPSILON`]. Lower is better; unlike Brier it keeps growing as a
/// wrong answer gets more confident, which is exactly the failure that deletes
/// something the household wanted.
pub fn log_loss(scores: &[f32], labels: &[f32]) -> f32 {
    if scores.is_empty() {
        return 0.0;
    }
    let total: f64 = scores
        .iter()
        .zip(labels)
        .map(|(score, label)| {
            let p = (*score as f64).clamp(LOG_LOSS_EPSILON, 1.0 - LOG_LOSS_EPSILON);
            if *label >= 0.5 {
                -p.ln()
            } else {
                -(1.0 - p).ln()
            }
        })
        .sum();
    (total / scores.len() as f64) as f32
}

/// Every headline metric for one model on one set of rows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Scorecard {
    pub n: usize,
    /// Rows labelled safe; `positives / n` is the base rate.
    pub positives: usize,
    pub auc: f32,
    pub brier: f32,
    pub log_loss: f32,
    pub ece: f32,
}

impl Scorecard {
    pub fn of(scores: &[f32], labels: &[f32]) -> Self {
        Self {
            n: scores.len(),
            positives: labels.iter().filter(|label| **label >= 0.5).count(),
            auc: auc(scores, labels),
            brier: brier(scores, labels),
            log_loss: log_loss(scores, labels),
            ece: ece(scores, labels, ECE_BINS),
        }
    }
}

/// Mean squared error of the probabilities. Lower is better; it punishes
/// confident mistakes hardest, which is what a deletion guard needs.
pub fn brier(scores: &[f32], labels: &[f32]) -> f32 {
    if scores.is_empty() {
        return 0.0;
    }
    scores.iter().zip(labels).map(|(score, label)| (score - label).powi(2)).sum::<f32>() / scores.len() as f32
}

/// Expected calibration error over equal-width bins: the average gap between the
/// probability a bin claims and the frequency it delivers.
pub fn ece(scores: &[f32], labels: &[f32], bins: usize) -> f32 {
    if scores.is_empty() || bins == 0 {
        return 0.0;
    }
    let mut total = 0.0f32;
    for bin in 0..bins {
        let low = bin as f32 / bins as f32;
        let high = (bin + 1) as f32 / bins as f32;
        let members: Vec<(f32, f32)> = scores
            .iter()
            .zip(labels)
            .filter(|(score, _)| {
                // Last bin is closed so a score of exactly 1.0 is counted.
                **score >= low && (**score < high || (bin == bins - 1 && **score <= high))
            })
            .map(|(score, label)| (*score, *label))
            .collect();
        if members.is_empty() {
            continue;
        }
        let claimed = members.iter().map(|(score, _)| score).sum::<f32>() / members.len() as f32;
        let observed = members.iter().map(|(_, label)| label).sum::<f32>() / members.len() as f32;
        total += (claimed - observed).abs() * members.len() as f32;
    }
    total / scores.len() as f32
}

/// How far a held-out score can be trusted. The validation panel is small, so
/// a point AUC or Brier alone overstates what it knows; the intervals say how
/// much it would move on another draw of titles. `confident_wrong` counts the
/// errors that cost the most: a stated 90% the wrong way round.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Spread {
    /// 95% bootstrap interval `[low, high]`; `None` when the panel is too thin.
    pub auc: Option<[f32; 2]>,
    pub brier: Option<[f32; 2]>,
    /// Rows forecast at ≥ 90% either way.
    pub confident: usize,
    /// Confident rows whose outcome went the other way.
    pub confident_wrong: usize,
}

/// 95% intervals for AUC and Brier by group bootstrap, plus confident-error
/// counts.
///
/// Resamples whole groups (library items), not rows: the panel asks about the
/// same title at several cut dates, and those rows move together. A row
/// bootstrap would treat them as independent evidence and report an interval
/// far narrower than the data supports. Deterministic ([`BOOTSTRAP_SEED`]), so
/// a report can be checked twice. Scores are P(safe); labels are 1.0 = safe.
pub fn spread(scores: &[f32], labels: &[f32], groups: &[&str]) -> Spread {
    let n = scores.len().min(labels.len()).min(groups.len());
    let (mut confident, mut confident_wrong) = (0, 0);
    for (score, label) in scores[..n].iter().zip(&labels[..n]) {
        let (says_safe, says_played) = (*score >= CONFIDENT, *score <= 1.0 - CONFIDENT);
        if says_safe || says_played {
            confident += 1;
            if (says_safe && *label < 0.5) || (says_played && *label >= 0.5) {
                confident_wrong += 1;
            }
        }
    }
    let (mut aucs, mut briers) = (Vec::with_capacity(BOOTSTRAP_RESAMPLES), Vec::with_capacity(BOOTSTRAP_RESAMPLES));
    let (mut drawn_scores, mut drawn_labels) = (Vec::with_capacity(n), Vec::with_capacity(n));
    let varied = resample_groups(&groups[..n], |rows| {
        drawn_scores.clear();
        drawn_labels.clear();
        drawn_scores.extend(rows.iter().map(|row| scores[*row]));
        drawn_labels.extend(rows.iter().map(|row| labels[*row]));
        briers.push(brier(&drawn_scores, &drawn_labels));
        if both_outcomes(&drawn_labels) {
            aucs.push(auc(&drawn_scores, &drawn_labels));
        }
    });
    if !varied {
        return Spread { auc: None, brier: None, confident, confident_wrong };
    }
    Spread { auc: interval(aucs), brier: interval(briers), confident, confident_wrong }
}

/// How one model scores against another on the same rows: 95% intervals of
/// `a` minus `b`, from the same group bootstrap, so both always face the same
/// titles. Negative Brier and log-loss differences, and a positive AUC
/// difference, favour `a`; an interval that spans zero is no clear difference.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Difference {
    pub brier: Option<[f32; 2]>,
    pub log_loss: Option<[f32; 2]>,
    pub auc: Option<[f32; 2]>,
}

pub fn paired_difference(a: &[f32], b: &[f32], labels: &[f32], groups: &[&str]) -> Difference {
    let n = a.len().min(b.len()).min(labels.len()).min(groups.len());
    let (mut briers, mut log_losses, mut aucs) = (Vec::new(), Vec::new(), Vec::new());
    let (mut drawn_a, mut drawn_b, mut drawn_labels) = (Vec::with_capacity(n), Vec::with_capacity(n), Vec::with_capacity(n));
    let varied = resample_groups(&groups[..n], |rows| {
        drawn_a.clear();
        drawn_b.clear();
        drawn_labels.clear();
        drawn_a.extend(rows.iter().map(|row| a[*row]));
        drawn_b.extend(rows.iter().map(|row| b[*row]));
        drawn_labels.extend(rows.iter().map(|row| labels[*row]));
        briers.push(brier(&drawn_a, &drawn_labels) - brier(&drawn_b, &drawn_labels));
        log_losses.push(log_loss(&drawn_a, &drawn_labels) - log_loss(&drawn_b, &drawn_labels));
        if both_outcomes(&drawn_labels) {
            aucs.push(auc(&drawn_a, &drawn_labels) - auc(&drawn_b, &drawn_labels));
        }
    });
    if !varied {
        return Difference::default();
    }
    Difference { brier: interval(briers), log_loss: interval(log_losses), auc: interval(aucs) }
}

/// Calls `each` with the rows of [`BOOTSTRAP_RESAMPLES`] resamples of whole
/// groups (library items), drawn with replacement. Returns `false`, without
/// calling it, when fewer than two groups leave nothing to vary.
/// Deterministic ([`BOOTSTRAP_SEED`]).
fn resample_groups(groups: &[&str], mut each: impl FnMut(&[usize])) -> bool {
    let mut members: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (row, group) in groups.iter().enumerate() {
        members.entry(*group).or_default().push(row);
    }
    if members.len() < 2 {
        return false;
    }
    let members: Vec<Vec<usize>> = members.into_values().collect();
    let mut rng = SplitMix64(BOOTSTRAP_SEED);
    let mut rows = Vec::with_capacity(groups.len());
    for _ in 0..BOOTSTRAP_RESAMPLES {
        rows.clear();
        for _ in 0..members.len() {
            rows.extend_from_slice(&members[rng.below(members.len())]);
        }
        each(&rows);
    }
    true
}

/// One class only: `auc` would answer 0.5, which is no measurement.
fn both_outcomes(labels: &[f32]) -> bool {
    let safe = labels.iter().filter(|label| **label >= 0.5).count();
    safe > 0 && safe < labels.len()
}

/// The 2.5th and 97.5th percentiles by nearest rank, or `None` when fewer
/// than half the resamples produced a value.
fn interval(mut values: Vec<f32>) -> Option<[f32; 2]> {
    if values.is_empty() || values.len() * 2 < BOOTSTRAP_RESAMPLES {
        return None;
    }
    values.sort_by(f32::total_cmp);
    let nearest_rank = |quantile: f64| {
        let rank = (quantile * values.len() as f64).ceil() as usize;
        values[rank.clamp(1, values.len()) - 1]
    };
    Some([nearest_rank(0.025), nearest_rank(0.975)])
}

/// SplitMix64 (Steele et al., 2014): tiny, seedable and plenty for resampling.
struct SplitMix64(u64);

impl SplitMix64 {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// Uniform in `0..bound` by multiply-high, with no modulo bias worth naming.
    fn below(&mut self, bound: usize) -> usize {
        ((self.next() as u128 * bound as u128) >> 64) as usize
    }
}

/// What the operating threshold does: of the items it flags, how many were
/// genuinely safe, and how many of the safe items it found.
pub struct ThresholdStats {
    pub flagged: usize,
    pub precision: f32,
    pub recall: f32,
}

pub fn at_threshold(scores: &[f32], labels: &[f32], floor: f32) -> ThresholdStats {
    let flagged: Vec<usize> = (0..scores.len()).filter(|i| scores[*i] >= floor).collect();
    let true_positives = flagged.iter().filter(|i| labels[**i] >= 0.5).count();
    let total_positives = labels.iter().filter(|label| **label >= 0.5).count();
    ThresholdStats {
        flagged: flagged.len(),
        precision: if flagged.is_empty() { 0.0 } else { true_positives as f32 / flagged.len() as f32 },
        recall: if total_positives == 0 { 0.0 } else { true_positives as f32 / total_positives as f32 },
    }
}

/// One-sided Clopper–Pearson upper bound on a binomial rate: the largest `p`
/// under which seeing at most `k` events in `n` trials is still `delta`-likely.
/// Exact, because at the counts this system sees (a handful of errors in a few
/// hundred rows) normal approximations are wrong in the dangerous direction.
pub fn binomial_upper_bound(k: usize, n: usize, delta: f64) -> f64 {
    if n == 0 || k >= n {
        return 1.0;
    }
    // P(X <= k) for X ~ Bin(n, p), accumulated term by term.
    let cdf = |p: f64| -> f64 {
        let mut term = (1.0 - p).powi(n as i32);
        let mut total = term;
        for i in 0..k {
            term *= (n - i) as f64 / (i + 1) as f64 * p / (1.0 - p);
            total += term;
        }
        total
    };
    let (mut low, mut high) = (k as f64 / n as f64, 1.0f64);
    for _ in 0..100 {
        let mid = (low + high) / 2.0;
        if cdf(mid) > delta {
            low = mid;
        } else {
            high = mid;
        }
    }
    high
}

/// A reclaim floor with a finite-sample guarantee behind it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CertifiedFloor {
    pub floor: f32,
    pub flagged: usize,
    pub false_reclaims: usize,
    pub upper_bound: f64,
}

/// The lowest floor whose false-reclaim risk is certified at level `alpha`.
///
/// Risk is the share of all items that would be reclaimed *and then played*:
/// E[1{score ≥ λ} · 1{played}]. It can only shrink as λ rises, so floors are
/// tested from the strictest down and the scan stops at the first failure —
/// fixed-sequence testing as in Learn-then-Test (Angelopoulos et al., 2021),
/// which needs no multiple-testing penalty. With probability ≥ 1 − δ the
/// returned floor keeps that risk ≤ α, provided the rows are exchangeable.
/// `None` means even the strictest floor cannot be vouched for with this much
/// data: a zero-error panel of n rows only certifies α ≥ 1 − δ^(1/n).
pub fn certified_floor(scores: &[f32], labels: &[f32], alpha: f64, delta: f64) -> Option<CertifiedFloor> {
    let n = scores.len();
    let mut best = None;
    for step in (50..=99).rev() {
        let floor = step as f32 / 100.0;
        let flagged = scores.iter().filter(|score| **score >= floor).count();
        let false_reclaims = scores.iter().zip(labels).filter(|(score, label)| **score >= floor && **label < 0.5).count();
        let upper_bound = binomial_upper_bound(false_reclaims, n, delta);
        if upper_bound > alpha {
            break;
        }
        best = Some(CertifiedFloor { floor, flagged, false_reclaims, upper_bound });
    }
    best
}

#[cfg(test)]
mod tests;
