//! Operator rules: hard constraints on what the plan may take, never a score.
//!
//! 0.3.0 removed a scoring rule engine: hand-tuned weights fought the regret
//! model and hid why an item left. These rules change only *feasibility*, so
//! every selected item still went for the least regret the constraints allow:
//!
//! - **`keep`** and an active **`keep_until`** make an item unselectable
//!   ([`Exclusion::Rule`]). `keep_until` holds for `days` after an event: the
//!   item was added, last played, or requested (by the scope's requesters when
//!   it names any).
//! - **`prefer_evict`** and **`must_evict`** set a [`Force`] the solver honours
//!   only on an item it could already select, and only on a volume that needs
//!   space: pins, partway protection, evidence gates, the grace period and
//!   Leaving Soon all still apply (see [`crate::plan::knapsack`]).
//! - **Keep beats evict.** An item both kinds match is kept, and reported as a
//!   [`Conflict`].
//! - **Missing facts keep.** A scope condition the facts cannot answer (no
//!   tags read, no Seerr, a season without a quality) is unknown: an unknown
//!   match applies a keep, never an evict, and is counted as uncertain.
//! - **Rolling retention** ([`retention`]): `keep_latest_seasons` keeps the
//!   newest seasons of a continuing show and `keep_first_season` its first.
//!   A season one of them keeps for certain leaves its show's order, so the
//!   seasons around it compete on their own: older seasons can go while the
//!   kept ones stay, which a plain keep would block.
//! - **Rules rank, they do not reset.** A [`Force`] set before the rules (the
//!   torrents' [`Force::Spare`]) stands unless a rule keeps or forces the item.

pub mod facts;
pub mod preview;
mod retention;
mod scope;

pub use facts::{Facts, RequestFact};
pub use scope::{Kind, Range, Scope};

use crate::plan::knapsack::Force;
use crate::plan::{Exclusion, MediaCandidate};
use scope::Truth;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// One operator rule (`settings.json` `rules[]`). Unknown keys are refused: a
/// misspelt scope key must not silently widen an evict rule to everything.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    /// Unique (case-insensitively): it names the rule wherever it keeps or
    /// forces an item.
    pub name: String,
    #[serde(default = "on")]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Scope::is_everything")]
    pub scope: Scope,
    pub effect: Effect,
}

fn on() -> bool {
    true
}

/// What a rule does to the items in its scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Effect {
    Keep,
    /// Keep for `days` after `after` happened; afterwards the rule is silent.
    KeepUntil {
        days: u32,
        after: Event,
    },
    /// Keep the newest `seasons` seasons (on disk, specials aside) of a
    /// continuing show; silent on an ended show and on a movie.
    KeepLatestSeasons {
        seasons: u32,
    },
    /// Keep a show's first regular season, so it can always be started.
    KeepFirstSeason,
    /// Taken before any item without a rule on its volume, only as far as the
    /// volume's target needs.
    PreferEvict,
    /// Taken whenever its volume needs space at all.
    MustEvict,
}

impl Effect {
    fn force(self) -> Option<Force> {
        match self {
            Self::PreferEvict => Some(Force::Prefer),
            Self::MustEvict => Some(Force::Must),
            Self::Keep | Self::KeepUntil { .. } | Self::KeepLatestSeasons { .. } | Self::KeepFirstSeason => None,
        }
    }

    /// A retention keep: the season it keeps leaves its show's order.
    fn unchains(self) -> bool {
        matches!(self, Self::KeepLatestSeasons { .. } | Self::KeepFirstSeason)
    }
}

/// The event a `keep_until` counts from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Event {
    Added,
    LastPlayed,
    Requested,
}

/// Rules past this many are refused: each one is evaluated on every item.
pub const MAX_RULES: usize = 200;

/// A rule list outside its bounds; the message names the rule and field.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct InvalidRule(pub String);

/// The bounds the rules editor enforces, held here too so a hand-edited file
/// or a scripted PUT cannot hand the planner a rule the page would refuse.
pub fn validate(rules: &[Rule]) -> Result<(), InvalidRule> {
    if rules.len() > MAX_RULES {
        return Err(InvalidRule(format!("at most {MAX_RULES} rules")));
    }
    let mut names = HashSet::new();
    for rule in rules {
        let name = rule.name.trim();
        if name.is_empty() || name.chars().count() > 80 {
            return Err(InvalidRule("every rule needs a name of 1 to 80 characters".to_owned()));
        }
        if !names.insert(name.to_lowercase()) {
            return Err(InvalidRule(format!("rule \u{201c}{name}\u{201d}: another rule has the same name")));
        }
        let refuse = |problem: &str| InvalidRule(format!("rule \u{201c}{name}\u{201d}: {problem}"));
        rule.scope.validate().map_err(|problem| refuse(&problem))?;
        match rule.effect {
            Effect::KeepUntil { days, .. } if !(1..=3_650).contains(&days) => return Err(refuse("keep_until days must be 1 to 3650")),
            Effect::KeepLatestSeasons { seasons } if !(1..=100).contains(&seasons) => {
                return Err(refuse("keep_latest_seasons seasons must be 1 to 100"))
            }
            Effect::PreferEvict | Effect::MustEvict if rule.scope.is_everything() => {
                return Err(refuse("an evict rule needs a scope: it would otherwise force the whole library"))
            }
            _ => {}
        }
    }
    Ok(())
}

/// Two rules disagree on one item; the keep won.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Conflict {
    pub id: String,
    pub title: String,
    pub keep: String,
    pub evict: String,
}

/// What one rule covered this run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuleCount {
    pub name: String,
    /// Items it applies to (a keep counts unknown matches too).
    pub items: usize,
    pub bytes: u64,
}

/// What the rules did to a candidate list.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Outcome {
    pub conflicts: Vec<Conflict>,
    /// One per enabled rule, in rule order.
    pub rules: Vec<RuleCount>,
    /// Items a rule kept only because a fact it asks about was missing.
    pub uncertain: usize,
    /// Items carrying a [`Force`] the planner may honour.
    pub forced: usize,
}

/// What the rules decide for one item.
#[derive(Debug, Clone, Default, PartialEq)]
struct Verdict<'r> {
    /// The deciding keep, how sure it is, and whether it unchains the item.
    keep: Option<(&'r str, Truth, bool)>,
    evict: Option<(&'r str, Force)>,
    /// Every enabled rule that applies, by index.
    applies: Vec<usize>,
}

fn judge<'r>(rules: &'r [Rule], candidate: &MediaCandidate, facts: &Facts, now: u64) -> Verdict<'r> {
    let mut verdict = Verdict::default();
    for (index, rule) in rules.iter().enumerate().filter(|(_, rule)| rule.enabled) {
        let mut truth = rule.scope.test(candidate, facts);
        match rule.effect {
            Effect::KeepUntil { days, after } => truth = truth.and(scope::within(after, days, &rule.scope, candidate, facts, now)),
            Effect::KeepLatestSeasons { seasons } => truth = truth.and(retention::latest(seasons, facts)),
            Effect::KeepFirstSeason => truth = truth.and(retention::first(facts)),
            Effect::Keep | Effect::PreferEvict | Effect::MustEvict => {}
        }
        match (rule.effect.force(), truth) {
            (None, Truth::Yes | Truth::Unknown) => {
                verdict.applies.push(index);
                // A certain keep names the item's reason over an uncertain
                // one, and a plain keep over a retention one: only a season
                // nothing else keeps may leave its show's order.
                let unchains = rule.effect.unchains() && truth == Truth::Yes;
                let better = |(_, held, held_unchains): (&str, Truth, bool)| {
                    (held == Truth::Unknown && truth == Truth::Yes) || (held == truth && held_unchains && !unchains)
                };
                if verdict.keep.is_none_or(better) {
                    verdict.keep = Some((&rule.name, truth, unchains));
                }
            }
            (Some(force), Truth::Yes) => {
                verdict.applies.push(index);
                // must beats prefer.
                if verdict.evict.is_none_or(|(_, held)| held == Force::Prefer && force == Force::Must) {
                    verdict.evict = Some((&rule.name, force));
                }
            }
            _ => {}
        }
    }
    verdict
}

/// Apply `rules` to every candidate: a keep excludes it (an earlier exclusion
/// stays its reason), an evict sets its [`Force`] unless it is protected or
/// already excluded. A candidate without facts is judged on what the
/// candidate itself knows.
pub fn apply(rules: &[Rule], candidates: &mut [MediaCandidate], facts: &HashMap<String, Facts>, now: u64) -> Outcome {
    let unknown = Facts::default();
    let mut outcome = Outcome {
        rules: rules.iter().filter(|rule| rule.enabled).map(|rule| RuleCount { name: rule.name.clone(), items: 0, bytes: 0 }).collect(),
        ..Outcome::default()
    };
    // Index of each enabled rule's count.
    let slot: HashMap<usize, usize> = rules.iter().enumerate().filter(|(_, rule)| rule.enabled).map(|(index, _)| index).zip(0..).collect();
    for candidate in candidates.iter_mut() {
        let verdict = judge(rules, candidate, facts.get(&candidate.id).unwrap_or(&unknown), now);
        for index in &verdict.applies {
            if let Some(count) = slot.get(index).and_then(|&slot| outcome.rules.get_mut(slot)) {
                count.items += 1;
                count.bytes = count.bytes.saturating_add(candidate.size_bytes);
            }
        }
        match (verdict.keep, verdict.evict) {
            (Some((keep, truth, unchains)), evict) => {
                if let Some((evict, _)) = evict {
                    outcome.conflicts.push(Conflict {
                        id: candidate.id.clone(),
                        title: candidate.title.clone(),
                        keep: keep.to_owned(),
                        evict: evict.to_owned(),
                    });
                }
                // Only a keep that decided the item counts as uncertain.
                if truth == Truth::Unknown && candidate.exclusion.is_none() {
                    outcome.uncertain += 1;
                }
                // An item excluded already keeps its place in its show.
                if unchains && candidate.exclusion.is_none() {
                    candidate.sequence = None;
                }
                candidate.force = None;
                candidate.exclusion = candidate.exclusion.take().or_else(|| Some(Exclusion::Rule(keep.to_owned())));
            }
            (None, Some((_, force))) if !candidate.protect && candidate.exclusion.is_none() => {
                candidate.force = Some(force);
                outcome.forced += 1;
            }
            (None, _) => {}
        }
    }
    outcome
}

/// `status.json` `rules`: what the operator's rules did this cycle. Conflicts
/// are capped so a broad pair of rules cannot bloat the status.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RulesStatus {
    pub rules: Vec<RuleCount>,
    pub conflicts: Vec<Conflict>,
    pub conflicts_total: usize,
    pub uncertain: usize,
    pub forced: usize,
}

/// Conflicts listed in the status; the rest are counted.
pub const STATUS_CONFLICTS: usize = 50;

impl RulesStatus {
    /// `None` without rules, so the status carries no empty block.
    pub fn of(rules: &[Rule], outcome: Outcome) -> Option<Self> {
        if rules.is_empty() {
            return None;
        }
        let conflicts_total = outcome.conflicts.len();
        let mut conflicts = outcome.conflicts;
        conflicts.truncate(STATUS_CONFLICTS);
        Some(Self { rules: outcome.rules, conflicts, conflicts_total, uncertain: outcome.uncertain, forced: outcome.forced })
    }
}

#[cfg(test)]
mod tests;
