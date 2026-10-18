//! The highlight's motion maths, free of GPUI and the clock: the session's
//! position integration, the precision damping, the steady release and the
//! position-report throttle. Every function takes the `now` it works at, so
//! tests drive real transitions.
//!
//! The effective gain of a step is `sensitivity / 100 * precision_factor`,
//! where the factor (only with `precision`) falls smoothly from 1 to
//! [`PRECISION_MIN_FACTOR`] as the smoothed hand speed drops from
//! [`PRECISION_FAST`] to [`PRECISION_SLOW`], so careful aiming moves the dot
//! in finer steps than a sweep.
//!
//! Letting go of a button jiggles the hand, which would drag the dot (and the
//! pointer warped to it) off the aimed target. With `steady_release` the
//! session therefore remembers its recent positions and, when it ends,
//! reports [`settled_position`] instead of the last one: the dwell point, but
//! only when the dot genuinely rested ([`DWELL_MIN`] within
//! [`DWELL_MAX_TRAVEL`] points) and then flicked away inside the final
//! [`FLICK_WINDOW`]. Continuous motion reports where the dot is.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use openlogi_core::binding::HighlightEffect;
use openlogi_core::config::{HighlightAppearance, HighlightMotion};
use openlogi_ipc::ActiveHighlight;

use crate::ipc::OverlayCommand;

/// Precision damping: the gain factor at (and below) [`PRECISION_SLOW`].
///
/// The Spotlight 2 reports ~100 Hz deltas of 1-3 units while aiming; at 0.45
/// that is a step of about one point per report, fine enough to land on a
/// small target, while the dot still starts moving at once. Hand-tuned on a
/// Spotlight 2.
const PRECISION_MIN_FACTOR: f32 = 0.45;
/// Hand speed, in device units per second, at and below which precision
/// damping is at full strength: 150 units/s is 1.5 units per ~100 Hz report,
/// the middle of what careful aiming produces (1-3 units). Hand-tuned on a
/// Spotlight 2.
const PRECISION_SLOW: f32 = 150.0;
/// Hand speed, in device units per second, at and above which the gain is
/// undamped: a sweep is 10-30 units per report, and 1000 units/s is 10 per
/// report. Between the two the factor follows a smoothstep, so it never
/// jumps. Hand-tuned on a Spotlight 2.
const PRECISION_FAST: f32 = 1000.0;
/// Time constant of the speed's exponential moving average, in seconds: about
/// six reports at ~100 Hz. Shorter lets a single 3-unit report of careful
/// aiming read as a sweep and undamp the dot; longer delays the transition
/// to full gain when a sweep starts. Hand-tuned on a Spotlight 2.
const SPEED_TIME_CONSTANT: f32 = 0.06;

/// How long before the release a flick may have started and still be undone:
/// the dwell must have ended inside this window.
const FLICK_WINDOW: Duration = Duration::from_millis(100);
/// The shortest stretch the pointer must stay put for to count as resting
/// before a release.
const DWELL_MIN: Duration = Duration::from_millis(150);
/// How far, in points, the dot may wander during a dwell.
const DWELL_MAX_TRAVEL: f32 = 3.0;
/// How much position history a session keeps: far more than the
/// [`FLICK_WINDOW`] plus [`DWELL_MIN`] a release looks back over.
const HISTORY_SPAN: Duration = Duration::from_secs(1);

/// The shortest gap between two position reports to the agent (~60 Hz). The
/// device reports at ~100 Hz and every report costs an IPC round trip plus a
/// pointer warp, which a display cannot show faster than this anyway.
const MOVE_INTERVAL: Duration = Duration::from_millis(16);

/// A drawn position at the moment it was reached.
type Sample = (Instant, (f32, f32));

/// The gain factor for a smoothed hand `speed` (device units per second):
/// [`PRECISION_MIN_FACTOR`] at and below [`PRECISION_SLOW`], 1 at and above
/// [`PRECISION_FAST`], a smoothstep between. Monotonic and continuous.
fn precision_factor(speed: f32) -> f32 {
    let t = ((speed - PRECISION_SLOW) / (PRECISION_FAST - PRECISION_SLOW)).clamp(0.0, 1.0);
    let eased = t * t * (3.0 - 2.0 * t);
    PRECISION_MIN_FACTOR + (1.0 - PRECISION_MIN_FACTOR) * eased
}

/// The gain of one step: the configured `sensitivity` (percent) scaled by the
/// precision `factor`. Screen points move per summed device unit.
fn effective_gain(sensitivity: u8, factor: f32) -> f32 {
    f32::from(sensitivity) / 100.0 * factor
}

/// Fold a speed `sample` into the moving average `previous` over `dt`
/// seconds: the weight of a new sample grows with the time it covers
/// (`1 - e^(-dt / T)`), so uneven observation gaps weigh correctly.
fn smooth_speed(previous: f32, sample: f32, dt: f32) -> f32 {
    let weight = 1.0 - (-dt / SPEED_TIME_CONSTANT).exp();
    previous + (sample - previous) * weight
}

/// Tracks the smoothed hand speed of a session for the precision factor.
struct Pace {
    /// Smoothed speed in device units per second. Starts at rest, so the
    /// first noisy report cannot undamp the dot on its own.
    speed: f32,
    /// When the previous observation was applied.
    last: Instant,
}

impl Pace {
    fn new(now: Instant) -> Self {
        Self {
            speed: 0.0,
            last: now,
        }
    }

    /// Fold an observation at `now` that moved the hand by `delta` device
    /// units, and return the resulting precision factor.
    fn factor(&mut self, now: Instant, delta: (f32, f32)) -> f32 {
        // A floor keeps two observations in one tick from dividing by ~0.
        let dt = now
            .saturating_duration_since(self.last)
            .max(Duration::from_millis(1))
            .as_secs_f32();
        self.last = now;
        self.speed = smooth_speed(self.speed, delta.0.hypot(delta.1) / dt, dt);
        precision_factor(self.speed)
    }
}

/// Where a session that ends at `released_at` should report its position:
/// the dot's resting point when the end of the gesture was a sudden flick off
/// it, else `last`.
///
/// `history` is oldest-first; each sample is the position the dot reached at
/// that time and kept until the next sample (or the release). Scanning back
/// from the release, the first sample that ends a dwell (see [`DWELL_MIN`],
/// [`DWELL_MAX_TRAVEL`]) wins if the dot left it no earlier than
/// [`FLICK_WINDOW`] before the release. A dwell that ended before that, or no
/// dwell at all (continuous motion), keeps `last`: there is no way to tell a
/// deliberate move from a flick, so nothing is rewound. An empty `history`
/// keeps `last` too.
fn settled_position(
    history: &VecDeque<Sample>,
    released_at: Instant,
    last: (f32, f32),
) -> (f32, f32) {
    let cutoff = released_at.checked_sub(FLICK_WINDOW);
    for end in (0..history.len()).rev() {
        let (_, rest) = history[end];
        // When the dot left this sample's position.
        let left_at = history.get(end + 1).map_or(released_at, |&(at, _)| at);
        if cutoff.is_some_and(|cutoff| left_at < cutoff) {
            break;
        }
        let mut start = end;
        while start > 0 && distance(history[start - 1].1, rest) < DWELL_MAX_TRAVEL {
            start -= 1;
        }
        if left_at.saturating_duration_since(history[start].0) >= DWELL_MIN {
            return rest;
        }
    }
    last
}

fn distance(a: (f32, f32), b: (f32, f32)) -> f32 {
    (a.0 - b.0).hypot(a.1 - b.1)
}

/// Rate limiter for position reports: at most one per [`MOVE_INTERVAL`], and
/// only the newest unsent one is kept, so a burst collapses into its last
/// position. Pure over the clock it is given.
#[derive(Default)]
pub(super) struct MoveThrottle {
    last_sent: Option<Instant>,
    pending: Option<OverlayCommand>,
}

impl MoveThrottle {
    /// Offer `command` at `now`: it comes back if it may be sent right away;
    /// otherwise it replaces any older unsent one and waits for
    /// [`flush`](Self::flush).
    pub(super) fn offer(
        &mut self,
        now: Instant,
        command: OverlayCommand,
    ) -> Option<OverlayCommand> {
        if self.ready(now) {
            self.pending = None;
            self.last_sent = Some(now);
            Some(command)
        } else {
            self.pending = Some(command);
            None
        }
    }

    /// How long until the pending report may be sent; `None` when nothing is
    /// pending.
    pub(super) fn wait(&self, now: Instant) -> Option<Duration> {
        self.pending?;
        Some(self.last_sent.map_or(Duration::ZERO, |sent| {
            (sent + MOVE_INTERVAL).saturating_duration_since(now)
        }))
    }

    /// The pending report, if the interval since the last send has passed.
    pub(super) fn flush(&mut self, now: Instant) -> Option<OverlayCommand> {
        if self.pending.is_none() || !self.ready(now) {
            return None;
        }
        self.last_sent = Some(now);
        self.pending.take()
    }

    /// Forget the pending report: its session is over.
    pub(super) fn clear(&mut self) {
        self.pending = None;
    }

    fn ready(&self, now: Instant) -> bool {
        self.last_sent
            .is_none_or(|sent| now.saturating_duration_since(sent) >= MOVE_INTERVAL)
    }
}

/// Where a press landed: its point in the display's local space and the
/// display's own global origin.
#[derive(Clone, Copy)]
pub(super) struct Anchor {
    pub(super) local: (f32, f32),
    pub(super) display_origin: (f64, f64),
}

/// `pos` (window-local points) in global top-left-origin points, given the
/// window's global origin.
fn to_global(pos: (f32, f32), display_origin: (f64, f64)) -> (f64, f64) {
    (
        display_origin.0 + f64::from(pos.0),
        display_origin.1 + f64::from(pos.1),
    )
}

fn clamp_into(pos: (f32, f32), size: (f32, f32)) -> (f32, f32) {
    (
        pos.0.clamp(0.0, size.0.max(0.0)),
        pos.1.clamp(0.0, size.1.max(0.0)),
    )
}

/// One highlight session as drawn.
pub(super) struct Session {
    pub(super) id: u64,
    pub(super) effect: HighlightEffect,
    /// How the effect is drawn; fixed at the start of the session.
    pub(super) appearance: HighlightAppearance,
    /// How the dot moves and settles; fixed at the start of the session.
    motion: HighlightMotion,
    /// Smoothed hand speed, for the precision factor.
    pace: Pace,
    /// Recent positions, oldest first, for the steady release; at most
    /// [`HISTORY_SPAN`] long (plus the one sample before it).
    history: VecDeque<Sample>,
    /// Window-local position of the dot / spotlight centre, in points.
    pub(super) pos: (f32, f32),
    /// Travel as of the last applied observation; deltas are taken from it.
    last_travel: (i64, i64),
    /// Global position of the window's top-left corner, when the press origin
    /// was known; without it the local position cannot be made global.
    display_origin: Option<(f64, f64)>,
}

impl Session {
    pub(super) fn new(
        highlight: &ActiveHighlight,
        anchor: Option<Anchor>,
        size: (f32, f32),
        now: Instant,
    ) -> Self {
        let pos = clamp_into(
            anchor.map_or((size.0 / 2.0, size.1 / 2.0), |anchor| anchor.local),
            size,
        );
        Self {
            id: highlight.session_id,
            effect: highlight.effect,
            appearance: highlight.appearance,
            motion: highlight.motion,
            pace: Pace::new(now),
            history: VecDeque::from([(now, pos)]),
            pos,
            last_travel: (highlight.travel_x, highlight.travel_y),
            display_origin: anchor.map(|anchor| anchor.display_origin),
        }
    }

    /// What to tell the agent when this session ends at `released_at`: its id
    /// and where the dot rested (the last drawn position unless the steady
    /// release undoes a flick) in global points. `None` when the position cannot
    /// be made global.
    pub(super) fn release_command(&self, released_at: Instant) -> Option<OverlayCommand> {
        let settled = if self.motion.steady_release {
            settled_position(&self.history, released_at, self.pos)
        } else {
            self.pos
        };
        let (x, y) = to_global(settled, self.display_origin?);
        Some(OverlayCommand::HighlightReleased {
            session_id: self.id,
            x,
            y,
        })
    }

    /// Where to tell the agent the dot is right now; `None` when the position
    /// cannot be made global.
    pub(super) fn move_command(&self) -> Option<OverlayCommand> {
        let (x, y) = to_global(self.pos, self.display_origin?);
        Some(OverlayCommand::HighlightMoved {
            session_id: self.id,
            x,
            y,
        })
    }

    /// Fold a continuation of this session, observed at `now`, into the
    /// position. Returns whether the position changed.
    pub(super) fn advance(
        &mut self,
        highlight: &ActiveHighlight,
        size: (f32, f32),
        now: Instant,
    ) -> bool {
        let before = self.pos;
        #[expect(
            clippy::cast_precision_loss,
            reason = "per-observation travel deltas are a few device units"
        )]
        let units = (
            (highlight.travel_x - self.last_travel.0) as f32,
            (highlight.travel_y - self.last_travel.1) as f32,
        );
        let factor = if self.motion.precision {
            self.pace.factor(now, units)
        } else {
            1.0
        };
        let gain = effective_gain(self.motion.sensitivity, factor);
        // Clamped per step: motion past an edge is discarded instead of
        // winding up and delaying the way back.
        self.pos = clamp_into(
            (self.pos.0 + units.0 * gain, self.pos.1 + units.1 * gain),
            size,
        );
        self.last_travel = (highlight.travel_x, highlight.travel_y);
        self.effect = highlight.effect;
        let moved = self.pos != before;
        if moved {
            self.remember(now);
        }
        moved
    }

    /// Record the current position as reached at `now`, dropping history
    /// older than [`HISTORY_SPAN`] but keeping the one sample that covers its
    /// start (a long rest is a single sample).
    fn remember(&mut self, now: Instant) {
        self.history.push_back((now, self.pos));
        let horizon = now.checked_sub(HISTORY_SPAN);
        while self.history.len() > 1 && horizon.is_some_and(|horizon| self.history[1].0 <= horizon)
        {
            self.history.pop_front();
        }
    }
}

#[cfg(test)]
mod tests;
