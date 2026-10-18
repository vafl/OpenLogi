//! Agent-owned presenter highlight state.
//!
//! While a presenter's highlight button is held, the agent owns the session:
//! which effect is bound, where the cursor was at the press, and how far the
//! device has moved since. The overlay only observes it, through its own
//! generation-stamped cell, and draws the result — it never sees the binding.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use openlogi_core::binding::HighlightEffect;
use openlogi_core::config::{Config, HighlightAppearance, HighlightMotion, HighlightPointer};
use openlogi_ipc::{ActiveHighlight, Generation, HighlightObservation, OBSERVE_HOLD};
use tokio::sync::watch;

use crate::runtime::pointer_output::{PointerOutput, PointerRequest};

/// Low bits of a session id that count the sessions of one agent run; the
/// bits above hold the agent's start time in milliseconds.
const SESSION_COUNTER_BITS: u32 = 20;

/// The first session id of an agent started at `start_millis` since the Unix
/// epoch. Ids restart with every agent run, and the overlay retries a late
/// `highlight_released` over a new connection, so a counter alone would let
/// that report match a session of the next run. A start time in the high bits
/// keeps the runs' ids apart for up to 2^20 sessions per run, and in the 53
/// bits a double represents exactly for centuries. The overlay treats ids as
/// opaque. Never `0`, which the pointer worker reserves as "no session".
fn first_session_id(start_millis: u64) -> u64 {
    (start_millis << SESSION_COUNTER_BITS) + 1
}

/// How one device's highlight looks and behaves, resolved from its config.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HighlightLook {
    /// What the visible pointer does while the highlight shows.
    pub pointer: HighlightPointer,
    /// How the overlay draws the effect.
    pub appearance: HighlightAppearance,
    /// How the effect moves with the device.
    pub motion: HighlightMotion,
}

impl HighlightLook {
    /// The look `config` resolves for the device under `config_key`.
    #[must_use]
    pub fn for_device(config: &Config, config_key: &str) -> Self {
        Self {
            pointer: config.presenter_pointer(config_key),
            appearance: config.presenter_appearance(config_key),
            motion: config.presenter_motion(config_key),
        }
    }
}

/// The one presenter highlight the agent can be showing, shared by the capture
/// session that feeds it and the IPC handler that serves it.
///
/// It only publishes state and enqueues pointer requests: the OS pointer is
/// driven by the [`PointerRuntime`](crate::runtime::pointer_output::PointerRuntime)
/// worker, which also decides which session a report belongs to.
pub struct HighlightSession {
    /// The next `session_id`, seeded by [`first_session_id`].
    next_session: AtomicU64,
    /// What the overlay observes. "No highlight" is the absence of an active
    /// session, so there is no release message to invent, and an overlay that
    /// restarts mid-press reads the live session instead of having missed its
    /// start.
    published: watch::Sender<HighlightObservation>,
    /// Hides, shows and moves the OS pointer on the pointer worker. Requests
    /// are submitted while the `published` lock is held, so they reach the
    /// worker in session order.
    pointer: PointerOutput,
    /// Where the overlay last reported a dot resting, so the next `Hide`
    /// session resumes there like an independent laser. `None` until the
    /// first release.
    laser: Mutex<Option<(f64, f64)>>,
}

impl HighlightSession {
    /// A session whose pointer requests go to `pointer`.
    #[must_use]
    pub fn new(pointer: PointerOutput) -> Self {
        let start_millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| u64::try_from(since.as_millis()).unwrap_or(0));
        Self::with_first_session_id(pointer, first_session_id(start_millis))
    }

    fn with_first_session_id(pointer: PointerOutput, first_session_id: u64) -> Self {
        // Generation 1: 0 is the observer's "seen nothing" sentinel.
        let (published, _) = watch::channel(HighlightObservation {
            generation: 1,
            active: None,
        });
        Self {
            next_session: AtomicU64::new(first_session_id),
            published,
            pointer,
            laser: Mutex::new(None),
        }
    }

    /// Start a new highlight at `origin` (global cursor position in
    /// top-left-origin points, `None` when unknown), replacing any active one.
    /// The session starts with zero travel and a fresh `session_id`; `look`
    /// is what happens to the visible pointer meanwhile (hidden, in the OS,
    /// from the first position the overlay reports until the session ends or
    /// is replaced; `Follow` never hides it), how
    /// the overlay draws the effect and how it moves.
    ///
    /// A `Hide` session starts where the last [`released`](Self::released)
    /// dot rested, `origin` only before any release; a `Follow` one always
    /// starts at `origin`, where the pointer was parked.
    pub fn begin(&self, effect: HighlightEffect, origin: Option<(f64, f64)>, look: HighlightLook) {
        let origin = match look.pointer {
            HighlightPointer::Hide => {
                (*self.laser.lock().unwrap_or_else(PoisonError::into_inner)).or(origin)
            }
            HighlightPointer::Follow => origin,
        };
        let session_id = self.next_session.fetch_add(1, Ordering::Relaxed);
        self.published.send_modify(|observed| {
            self.pointer.submit(PointerRequest::Begin {
                session_id,
                pointer: look.pointer,
            });
            observed.active = Some(ActiveHighlight {
                session_id,
                effect,
                origin,
                travel_x: 0,
                travel_y: 0,
                pointer: look.pointer,
                appearance: look.appearance,
                motion: look.motion,
            });
            observed.generation += 1;
        });
    }

    /// Add one raw-XY delta to the active session's travel. A no-op while no
    /// highlight is active (and for a zero delta), so a stray motion report
    /// after the release cannot resurrect or wake anything.
    pub fn travel(&self, dx: i32, dy: i32) {
        if dx == 0 && dy == 0 {
            return;
        }
        self.published.send_if_modified(|observed| {
            let Some(active) = observed.active.as_mut() else {
                return false;
            };
            active.travel_x = active.travel_x.saturating_add(i64::from(dx));
            active.travel_y = active.travel_y.saturating_add(i64::from(dy));
            observed.generation += 1;
            true
        });
    }

    /// Switch the active session to `effect`, keeping its `session_id`,
    /// origin and travel. A no-op while none is active or when the effect
    /// already matches.
    pub fn set_effect(&self, effect: HighlightEffect) {
        self.published.send_if_modified(|observed| {
            let Some(active) = observed.active.as_mut() else {
                return false;
            };
            if active.effect == effect {
                return false;
            }
            active.effect = effect;
            observed.generation += 1;
            true
        });
    }

    /// End the active session and show the pointer again. A no-op while none
    /// is active.
    pub fn end(&self) {
        self.published.send_if_modified(|observed| {
            if observed.active.take().is_none() {
                return false;
            }
            self.pointer.submit(PointerRequest::End);
            observed.generation += 1;
            true
        });
    }

    /// The overlay's report that session `session_id` ended with its dot at
    /// `(x, y)` (global top-left-origin points): ask the pointer worker to
    /// move the pointer there so the next press continues from it and clicks
    /// land under the dot. Only a [`HighlightPointer::Follow`] session moves
    /// the pointer; a `Hide` one leaves it where it was.
    ///
    /// The worker ignores it unless `session_id` is the most recently ended
    /// session and no session is active by then: a late report for an older
    /// session, or one that races a new press, must not yank the pointer.
    ///
    /// The position is also where the next `Hide` session starts its dot.
    pub fn released(&self, session_id: u64, x: f64, y: f64) {
        *self.laser.lock().unwrap_or_else(PoisonError::into_inner) = Some((x, y));
        self.pointer
            .submit(PointerRequest::Released { session_id, x, y });
    }

    /// The overlay's running report that session `session_id` has its dot at
    /// `(x, y)` (same space as [`released`](Self::released)): ask the pointer
    /// worker to move the pointer there, so a click during the highlight
    /// lands under the dot. Only a [`HighlightPointer::Follow`] session moves
    /// the pointer; a `Hide` one only hides it, from its first report on.
    ///
    /// The worker ignores it unless `session_id` is the session active by
    /// then; a late report from a session that already ended must not yank the
    /// pointer (its final position is the release's business). Consecutive
    /// reports coalesce to the newest.
    pub fn moved(&self, session_id: u64, x: f64, y: f64) {
        self.pointer
            .submit(PointerRequest::Moved { session_id, x, y });
    }

    /// The highlight being shown right now, if any.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn current(&self) -> Option<ActiveHighlight> {
        self.published.borrow().active
    }

    /// Serve one [`Agent::observe_highlight`](openlogi_ipc::Agent::observe_highlight).
    pub async fn observe(&self, since: Generation) -> HighlightObservation {
        let mut rx = self.published.subscribe();
        let changed = rx.wait_for(|observed| observed.generation != since);
        match tokio::time::timeout(OBSERVE_HOLD, changed).await {
            Ok(Ok(observed)) => *observed,
            // Hold elapsed, or the session is gone: answer with what we have.
            Ok(Err(_)) | Err(_) => *self.published.borrow(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, mpsc};

    use super::*;
    use crate::runtime::pointer_output::{PointerCommand, recorded_requests};

    fn hide() -> HighlightLook {
        HighlightLook::default()
    }

    fn follow() -> HighlightLook {
        HighlightLook {
            pointer: HighlightPointer::Follow,
            ..HighlightLook::default()
        }
    }

    /// A session whose pointer requests are recorded instead of reaching a worker.
    fn session() -> (HighlightSession, mpsc::Receiver<PointerCommand>) {
        let (pointer, recorded) = PointerOutput::recording();
        (
            HighlightSession::with_first_session_id(pointer, 1),
            recorded,
        )
    }

    #[test]
    fn session_ids_of_a_later_agent_run_start_above_those_of_an_earlier_one() {
        let first_run_end = first_session_id(1_000) + (1 << SESSION_COUNTER_BITS) - 2;
        assert!(first_session_id(1_001) > first_run_end);
        assert_ne!(first_session_id(0), 0, "0 is reserved for no session");
        let live = HighlightSession::new(PointerOutput::detached());
        live.begin(HighlightEffect::LaserDot, None, hide());
        let id = live.current().expect("active").session_id;
        assert!(id > 1 << SESSION_COUNTER_BITS, "seeded with the start time");
    }

    fn begin(id: u64, pointer: HighlightPointer) -> PointerRequest {
        PointerRequest::Begin {
            session_id: id,
            pointer,
        }
    }

    #[test]
    fn the_pointer_requests_follow_the_session() {
        let (session, pointer) = session();
        session.begin(HighlightEffect::LaserDot, None, hide());
        session.end();
        session.end();
        assert_eq!(
            recorded_requests(&pointer),
            [begin(1, HighlightPointer::Hide), PointerRequest::End],
            "a second end has nothing to end"
        );

        session.begin(HighlightEffect::LaserDot, None, follow());
        session.end();
        assert_eq!(
            recorded_requests(&pointer),
            [begin(2, HighlightPointer::Follow), PointerRequest::End],
            "Follow never hides"
        );

        session.begin(HighlightEffect::LaserDot, None, hide());
        session.begin(HighlightEffect::LaserDot, None, follow());
        assert_eq!(
            recorded_requests(&pointer),
            [
                begin(3, HighlightPointer::Hide),
                begin(4, HighlightPointer::Follow)
            ],
            "a replacing session is a begin of its own"
        );
    }

    #[test]
    fn reports_are_forwarded_to_the_pointer_worker_to_judge() {
        let (session, pointer) = session();
        session.moved(7, 1.0, 2.0);
        session.released(7, 3.0, 4.0);
        assert_eq!(
            recorded_requests(&pointer),
            [
                PointerRequest::Moved {
                    session_id: 7,
                    x: 1.0,
                    y: 2.0
                },
                PointerRequest::Released {
                    session_id: 7,
                    x: 3.0,
                    y: 4.0
                },
            ]
        );
    }

    #[test]
    fn a_detached_session_never_reaches_the_pointer() {
        let session = HighlightSession::new(PointerOutput::detached());
        session.begin(HighlightEffect::LaserDot, None, hide());
        session.moved(1, 1.0, 1.0);
        session.end();
        assert_eq!(session.current(), None);
    }

    #[test]
    fn begin_travel_and_end_walk_the_session() {
        let (session, _) = session();
        assert_eq!(session.current(), None);

        session.begin(HighlightEffect::LaserDot, Some((10.0, 20.0)), follow());
        let first = session.current().expect("begin starts a session");
        assert_eq!(first.effect, HighlightEffect::LaserDot);
        assert_eq!(first.origin, Some((10.0, 20.0)));
        assert_eq!(first.pointer, HighlightPointer::Follow);
        assert_eq!((first.travel_x, first.travel_y), (0, 0));

        session.travel(3, -4);
        session.travel(-1, 10);
        let moved = session.current().expect("still active");
        assert_eq!((moved.travel_x, moved.travel_y), (2, 6));
        assert_eq!(moved.session_id, first.session_id);

        session.end();
        assert_eq!(session.current(), None);
    }

    #[test]
    fn a_hide_session_resumes_where_the_last_release_left_the_dot() {
        let (session, _) = session();
        let origin = |session: &HighlightSession| session.current().expect("active").origin;

        session.begin(HighlightEffect::LaserDot, Some((10.0, 20.0)), hide());
        assert_eq!(origin(&session), Some((10.0, 20.0)), "no release yet");
        session.moved(1, 99.0, 99.0);
        session.end();
        session.begin(HighlightEffect::LaserDot, Some((11.0, 21.0)), hide());
        assert_eq!(origin(&session), Some((11.0, 21.0)), "a move is no release");

        session.end();
        session.released(2, 300.0, 400.0);
        session.begin(HighlightEffect::LaserDot, Some((12.0, 22.0)), hide());
        assert_eq!(origin(&session), Some((300.0, 400.0)));
        session.begin(HighlightEffect::LaserDot, None, hide());
        assert_eq!(
            origin(&session),
            Some((300.0, 400.0)),
            "even without a cursor"
        );

        session.begin(HighlightEffect::LaserDot, Some((12.0, 22.0)), follow());
        assert_eq!(
            origin(&session),
            Some((12.0, 22.0)),
            "Follow ignores the memory"
        );
    }

    #[test]
    fn set_effect_keeps_the_session_and_its_travel() {
        let (session, _) = session();
        session.set_effect(HighlightEffect::Spotlight);
        assert_eq!(session.current(), None, "no session to retarget");

        session.begin(HighlightEffect::LaserDot, Some((1.0, 2.0)), hide());
        session.travel(3, 4);
        let before = session.current().expect("active");
        let generation = *session.published.borrow();
        session.set_effect(HighlightEffect::LaserDot);
        assert_eq!(
            *session.published.borrow(),
            generation,
            "same effect is a no-op"
        );

        session.set_effect(HighlightEffect::Spotlight);
        let after = session.current().expect("still active");
        assert_eq!(after.effect, HighlightEffect::Spotlight);
        assert_eq!(after.session_id, before.session_id);
        assert_eq!(after.origin, before.origin);
        assert_eq!((after.travel_x, after.travel_y), (3, 4));
        assert_eq!(
            session.published.borrow().generation,
            generation.generation + 1
        );
    }

    #[test]
    fn a_new_press_replaces_the_old_one_with_a_fresh_id() {
        let (session, _) = session();
        session.begin(HighlightEffect::LaserDot, None, hide());
        session.travel(5, 5);
        let first = session.current().expect("active");
        session.begin(HighlightEffect::Spotlight, None, hide());
        let second = session.current().expect("active");
        assert_ne!(first.session_id, second.session_id);
        assert_eq!(second.effect, HighlightEffect::Spotlight);
        assert_eq!((second.travel_x, second.travel_y), (0, 0));
    }

    #[test]
    fn travel_and_end_without_a_session_change_nothing() {
        let (session, pointer) = session();
        let before = *session.published.borrow();
        session.travel(7, 7);
        session.end();
        assert_eq!(*session.published.borrow(), before);
        assert_eq!(recorded_requests(&pointer), []);
    }

    #[tokio::test]
    async fn every_change_bumps_the_generation() {
        let (session, _) = session();
        let g0 = session.observe(0).await.generation;
        session.begin(HighlightEffect::Spotlight, None, hide());
        session.travel(1, 1);
        session.end();
        let after = session.observe(g0).await;
        assert_eq!(after.generation, g0 + 3);
        assert_eq!(after.active, None);
    }

    #[tokio::test]
    async fn a_stale_generation_answers_at_once_with_the_live_session() {
        let (session, _) = session();
        session.begin(HighlightEffect::LaserDot, Some((1.0, 2.0)), hide());
        // Generation 0 is "seen nothing": an overlay restarted mid-press picks
        // up the live session.
        let observed = session.observe(0).await;
        assert_eq!(observed.active, session.current());
        assert!(observed.active.is_some());
    }

    #[tokio::test(start_paused = true)]
    async fn an_unchanged_generation_holds_then_answers_unchanged() {
        let (session, _) = session();
        let seen = session.observe(0).await;
        let held = session.observe(seen.generation).await;
        assert_eq!(held, seen, "the heartbeat repeats the unchanged state");
    }

    #[tokio::test]
    async fn a_pending_observe_is_woken_by_a_change() {
        let session = Arc::new(session().0);
        let seen = session.observe(0).await;
        let waiter = tokio::spawn({
            let session = Arc::clone(&session);
            async move { session.observe(seen.generation).await }
        });
        tokio::task::yield_now().await;
        session.begin(HighlightEffect::Spotlight, None, hide());
        let woken = waiter.await.expect("observe task");
        assert_eq!(woken.generation, seen.generation + 1);
        assert_eq!(
            woken.active.map(|active| active.effect),
            Some(HighlightEffect::Spotlight)
        );
    }
}
