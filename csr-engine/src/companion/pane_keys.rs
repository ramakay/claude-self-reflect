//! The pane's act keys, read: moved out of pane.rs (over the 2,000-line cap) to make room there.
use super::render::observation::ObsCard;

/// The card and place an `observe:<card>:<index>` act names.
pub(crate) fn observe_key(key: &str) -> Option<(ObsCard, usize)> {
    let (card, index) = key.strip_prefix("observe:")?.split_once(':')?;
    let card = match card.parse::<usize>().ok()? {
        0 => ObsCard::Confusing,
        1 => ObsCard::ShouldBe,
        3 => ObsCard::Lost,
        4 => ObsCard::Handed,
        _ => ObsCard::More,
    };
    Some((card, index.parse().ok()?))
}
