//! Which copy to keep. The criteria, most decisive first:
//! 1. A copy an *arr tracks: removing it only makes the *arr download it
//!    again.
//! 2. A copy the household played: plays carry the ratingKey.
//! 3. Picture: the highest, unless the preference is HD or quality advice
//!    says downgrade (advice to keep the original restores the highest).
//! 4. Size: the larger file at the highest picture, the smaller under HD.
//! 5. The lowest copy id, so the answer is stable.

use super::{Copy, KeepPreference};

/// The quality advice for the item ([`crate::quality::QualityAction`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Advice {
    #[default]
    None,
    Downgrade,
    KeepOriginal,
}

impl Advice {
    pub fn of(action: &crate::quality::QualityAction) -> Self {
        match action {
            crate::quality::QualityAction::DowngradeQuality { .. } => Self::Downgrade,
            crate::quality::QualityAction::KeepOriginal { .. } => Self::KeepOriginal,
            crate::quality::QualityAction::EligibleForEviction => Self::None,
        }
    }
}

/// Rank of a picture: higher is preferred.
fn picture(resolution: Option<&str>, hd: bool) -> u8 {
    match (resolution, hd) {
        (Some("2160"), false) | (Some("1080"), true) => 4,
        (Some("1080"), false) | (Some("720"), true) => 3,
        (Some("720"), false) | (Some("2160"), true) => 2,
        (Some(_), _) => 1,
        (None, _) => 0,
    }
}

fn key(copy: &Copy, hd: bool) -> (bool, bool, u8, u64) {
    let size = if hd { u64::MAX - copy.bytes } else { copy.bytes };
    (copy.owner.is_some(), copy.plays > 0, picture(copy.resolution.as_deref(), hd), size)
}

/// The id of the copy to keep, and why. `copies` is never empty in a group;
/// an empty slice recommends nothing.
pub fn recommend(copies: &[Copy], prefer: KeepPreference, advice: Advice) -> (String, Vec<String>) {
    let hd = match advice {
        Advice::KeepOriginal => false,
        Advice::Downgrade => true,
        Advice::None => prefer == KeepPreference::Hd,
    };
    let mut best: Option<&Copy> = None;
    for copy in copies {
        best = match best {
            Some(current) if key(current, hd) > key(copy, hd) => Some(current),
            Some(current) if key(current, hd) == key(copy, hd) && current.id <= copy.id => Some(current),
            _ => Some(copy),
        };
    }
    let Some(best) = best else { return (String::new(), Vec::new()) };
    let others: Vec<&Copy> = copies.iter().filter(|copy| copy.id != best.id).collect();
    let mut reasons = Vec::new();
    if let Some(owner) = &best.owner {
        if others.iter().any(|copy| copy.owner.is_none()) {
            reasons.push(format!("{} tracks this copy: removing it would only download it again", owner.instance));
        }
    }
    if best.plays > 0 && others.iter().any(|copy| copy.plays == 0) {
        reasons.push(format!("the household played this copy ({} play(s))", best.plays));
    }
    let rank = picture(best.resolution.as_deref(), hd);
    if others.iter().any(|copy| picture(copy.resolution.as_deref(), hd) < rank) {
        let picture =
            best.resolution.as_deref().map_or("unknown".to_string(), |res| if res == "sd" { "SD".into() } else { format!("{res}p") });
        let why = match advice {
            Advice::Downgrade => "quality advice says downgrade, so HD is preferred",
            Advice::KeepOriginal => "quality advice keeps the original picture",
            Advice::None if hd => "the preference is HD",
            Advice::None => "the preference is the highest picture",
        };
        reasons.push(format!("{picture}: {why}"));
    }
    if reasons.is_empty() && others.iter().any(|copy| copy.bytes != best.bytes) {
        reasons.push(if hd { "the smaller file".to_string() } else { "the larger file".to_string() });
    }
    if reasons.is_empty() {
        reasons.push("the copies are alike; the first one listed".to_string());
    }
    (best.id.clone(), reasons)
}
