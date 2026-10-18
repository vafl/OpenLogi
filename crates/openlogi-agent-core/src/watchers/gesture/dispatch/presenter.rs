//! Presenter highlight holds: which presenter buttons each capture session
//! holds, and the highlight transition each edge stands for.
//!
//! Sans-I/O: [`PresenterHolds`] only decides; the dispatcher applies the
//! returned [`HighlightStep`] to the shared highlight session.

use std::collections::HashMap;

use openlogi_core::binding::{ButtonId, HighlightEffect};

use crate::runtime::HidppSessionId;

/// What one presenter edge does to the shared highlight.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum HighlightStep {
    /// The first presenter button went down: start a highlight.
    Begin(HighlightEffect),
    /// A presenter button joined or left a hold: retarget the effect.
    SetEffect(HighlightEffect),
    /// The last presenter button was released.
    End,
}

/// Held highlight-bound presenter buttons per capture session, in press order.
///
/// The accumulator delivers a force swap as Down then Up in one report, so the
/// highlight retargets; Up then Down would end and begin a new session (fresh
/// id, travel back at zero), and the dispatcher sees one edge at a time, so it
/// cannot defer the end.
#[derive(Default)]
pub(super) struct PresenterHolds {
    held: HashMap<HidppSessionId, Vec<(ButtonId, HighlightEffect)>>,
}

impl PresenterHolds {
    /// A highlight-bound presenter button went down.
    pub(super) fn down(
        &mut self,
        session: &HidppSessionId,
        button: ButtonId,
        effect: HighlightEffect,
    ) -> HighlightStep {
        let holds = self.held.entry(session.clone()).or_default();
        let first = holds.is_empty();
        holds.retain(|&(held, _)| held != button);
        holds.push((button, effect));
        if first {
            HighlightStep::Begin(effect)
        } else {
            HighlightStep::SetEffect(effect)
        }
    }

    /// A presenter button came up. `None` when it was not a tracked
    /// highlight hold (an ordinary remap, or a stray release).
    pub(super) fn up(
        &mut self,
        session: &HidppSessionId,
        button: ButtonId,
    ) -> Option<HighlightStep> {
        let holds = self.held.get_mut(session)?;
        let before = holds.len();
        holds.retain(|&(held, _)| held != button);
        if holds.len() == before {
            return None;
        }
        // The newest remaining hold wins, as it does in `down`.
        let step = holds.last().map_or(HighlightStep::End, |&(_, effect)| {
            HighlightStep::SetEffect(effect)
        });
        if holds.is_empty() {
            self.held.remove(session);
        }
        Some(step)
    }

    /// Whether `session` holds a highlight, so raw motion belongs to it.
    pub(super) fn is_holding(&self, session: &HidppSessionId) -> bool {
        self.held.contains_key(session)
    }

    /// Forget `session`'s holds; `true` when it held a highlight that must
    /// now end.
    pub(super) fn cancel_session(&mut self, session: &HidppSessionId) -> bool {
        self.held.remove(session).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LASER: HighlightEffect = HighlightEffect::LaserDot;
    const SPOT: HighlightEffect = HighlightEffect::Spotlight;

    fn session(epoch: u64) -> HidppSessionId {
        HidppSessionId::with_epoch("spotlight", epoch)
    }

    #[test]
    fn overlapping_holds_retarget_and_end_with_the_last_release() {
        let mut holds = PresenterHolds::default();
        let s = session(1);
        assert_eq!(
            holds.down(&s, ButtonId::PresenterTouch, LASER),
            HighlightStep::Begin(LASER)
        );
        assert_eq!(
            holds.down(&s, ButtonId::PresenterPress, SPOT),
            HighlightStep::SetEffect(SPOT)
        );
        assert_eq!(
            holds.up(&s, ButtonId::PresenterPress),
            Some(HighlightStep::SetEffect(LASER))
        );
        assert_eq!(
            holds.up(&s, ButtonId::PresenterTouch),
            Some(HighlightStep::End)
        );
        assert!(!holds.is_holding(&s));
    }

    #[test]
    fn a_force_swap_is_down_then_up_and_keeps_the_session() {
        let mut holds = PresenterHolds::default();
        let s = session(1);
        holds.down(&s, ButtonId::PresenterTouch, LASER);
        assert_eq!(
            holds.down(&s, ButtonId::PresenterPress, SPOT),
            HighlightStep::SetEffect(SPOT)
        );
        assert_eq!(
            holds.up(&s, ButtonId::PresenterTouch),
            Some(HighlightStep::SetEffect(SPOT)),
            "down-then-up retargets"
        );
        assert!(holds.is_holding(&s));

        let mut reversed = PresenterHolds::default();
        reversed.down(&s, ButtonId::PresenterTouch, LASER);
        assert_eq!(
            reversed.up(&s, ButtonId::PresenterTouch),
            Some(HighlightStep::End),
            "up-then-down would end the session first"
        );
        assert_eq!(
            reversed.down(&s, ButtonId::PresenterPress, SPOT),
            HighlightStep::Begin(SPOT)
        );
    }

    #[test]
    fn releasing_the_earlier_button_first_keeps_the_later_effect() {
        let mut holds = PresenterHolds::default();
        let s = session(1);
        holds.down(&s, ButtonId::PresenterTouch, LASER);
        holds.down(&s, ButtonId::PresenterPress, SPOT);
        assert_eq!(
            holds.up(&s, ButtonId::PresenterTouch),
            Some(HighlightStep::SetEffect(SPOT))
        );
        assert_eq!(
            holds.up(&s, ButtonId::PresenterPress),
            Some(HighlightStep::End)
        );
    }

    #[test]
    fn the_newest_remaining_hold_decides_the_effect() {
        let mut holds = PresenterHolds::default();
        let s = session(1);
        holds.down(&s, ButtonId::PresenterTouch, LASER);
        holds.down(&s, ButtonId::PresenterPress, SPOT);
        holds.down(&s, ButtonId::PresenterClick, LASER);
        assert_eq!(
            holds.up(&s, ButtonId::PresenterClick),
            Some(HighlightStep::SetEffect(SPOT)),
            "the newest of the rest, not the oldest"
        );
        assert_eq!(
            holds.up(&s, ButtonId::PresenterPress),
            Some(HighlightStep::SetEffect(LASER))
        );
        assert_eq!(
            holds.up(&s, ButtonId::PresenterTouch),
            Some(HighlightStep::End)
        );
    }

    #[test]
    fn an_untracked_release_is_left_to_the_ordinary_path() {
        let mut holds = PresenterHolds::default();
        let s = session(1);
        assert_eq!(holds.up(&s, ButtonId::PresenterTouch), None);
        holds.down(&s, ButtonId::PresenterTouch, LASER);
        assert_eq!(holds.up(&s, ButtonId::PresenterPress), None);
        assert!(holds.is_holding(&s));
    }

    #[test]
    fn cancelling_a_session_ends_only_its_own_highlight() {
        let mut holds = PresenterHolds::default();
        holds.down(&session(1), ButtonId::PresenterTouch, LASER);
        assert!(!holds.cancel_session(&session(2)));
        assert!(holds.is_holding(&session(1)));
        assert!(holds.cancel_session(&session(1)));
        assert!(!holds.cancel_session(&session(1)));
    }
}
