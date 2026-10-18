//! The presenter highlight: a laser dot or a spotlight that follows the
//! device while its button is held.
//!
//! The agent owns the session and only publishes it ([`HighlightObservation`]);
//! this module owns the one transparent, click-through window the highlight is
//! drawn in, and [`motion`] owns the motion integration. The window is created
//! on the first session, so overlay users who never present never get one, and
//! kept afterwards so later presses do not wait for window creation: while no
//! session is active it paints nothing and is never repainted. A new
//! `session_id` reuses it when it covers the press's display (same id, same
//! size) and recreates it otherwise; a hidden observation clears the session
//! and keeps the window for the rest of the run. The window covers one display
//! only, the one under the press, so the dot clamps to it and does not follow
//! the pointer onto another display.
//!
//! The highlight window is macOS-only for now ([`HIGHLIGHT_OVERLAY_SUPPORTED`]): its
//! click-through and above-full-screen policy is
//! `platform::configure_highlight_window`, which has no other OS behind it
//! yet. The window fails closed: a persistent full-display window without
//! input passthrough would swallow every click, so a window whose policy
//! could not be applied is removed at once and recreated by the next session.
//! Elsewhere the observation loop still runs and drains the agent's updates,
//! so a port only needs the window policy.
//!
//! The position is reported to the agent while the session runs (throttled,
//! newest wins) and once more on release; the agent keeps the OS pointer under
//! the dot only for [`HighlightPointer::Follow`], and hides it, without moving
//! it, for [`HighlightPointer::Hide`]. The window is click-through at all
//! times; hiding is the agent's business, as only the process that synthesises
//! input can hide the pointer from the background.
//!
//! [`HighlightPointer::Follow`]: openlogi_core::config::HighlightPointer::Follow
//! [`HighlightPointer::Hide`]: openlogi_core::config::HighlightPointer::Hide

mod motion;

use std::time::{Duration, Instant};

use gpui::{
    AppContext as _, Bounds, Context, Hsla, IntoElement, ParentElement as _, Pixels, Render, Size,
    Styled as _, Window, WindowHandle, WindowOptions, canvas, div, fill, point, px, quad, rgb,
};
use openlogi_core::binding::HighlightEffect;
use openlogi_core::config::{HighlightAppearance, LaserSize};
use openlogi_core::device::HIGHLIGHT_OVERLAY_SUPPORTED;
use openlogi_ipc::{ActiveHighlight, HighlightObservation};
use tokio::sync::mpsc;
use tracing::warn;

use self::motion::{Anchor, MoveThrottle, Session};
use crate::ipc::{HighlightUpdate, OverlayCommand};
use crate::platform::{Located, NativeView, locate, native_view};
use crate::ring::ring_window_options;

/// The radius of the laser dot the glow discs of [`DOT_GLOW`] are specified
/// for.
const GLOW_BASE_RADIUS: f32 = LaserSize::DEFAULT.into_inner() as f32;

/// The laser dot's two soft glow discs painted beneath the solid one, as
/// (radius, alpha) for a dot of [`GLOW_BASE_RADIUS`] points. The radii scale
/// with the configured dot size; the alphas do not. Two discs at about 2.7x
/// and 1.8x the dot radius give a halo that stays visible on a projector
/// without reading as a second, larger dot; hand-tuned on a Spotlight 2.
const DOT_GLOW: [(f32, f32); 2] = [(32.0, 0.10), (22.0, 0.18)];

/// The spotlight: how many stacked layers (one pixel-ring [`FEATHER_STEP`]
/// apart) soften the edge of the clear circle. Four layers make the falloff
/// read as smooth at presentation distance; more only add overdraw.
/// Hand-tuned on a Spotlight 2. The radius and the dimming outside the circle
/// are the configured [`HighlightAppearance`].
const FEATHER_LAYERS: u32 = 4;
/// Radius growth, in points, from one feather layer to the next: with
/// [`FEATHER_LAYERS`] a ~9 pt soft edge. Hand-tuned on a Spotlight 2.
const FEATHER_STEP: f32 = 3.0;

/// What the persistent window shows: at most one session.
pub(crate) struct HighlightView {
    session: Option<Session>,
}

impl Render for HighlightView {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        // Idle: nothing to draw, and nothing requests another frame.
        let shown = self
            .session
            .as_ref()
            .map(|session| (session.effect, session.pos, session.appearance));
        div().size_full().child(
            canvas(
                |_, _, _| (),
                move |_, (), window, _| {
                    let Some((effect, pos, appearance)) = shown else {
                        return;
                    };
                    match effect {
                        HighlightEffect::LaserDot => paint_laser_dot(window, pos, appearance),
                        HighlightEffect::Spotlight => {
                            let size = window.viewport_size();
                            paint_spotlight(window, pos, size, appearance);
                        }
                    }
                },
            )
            .size_full(),
        )
    }
}

fn disc(center: (f32, f32), radius: f32) -> Bounds<Pixels> {
    Bounds::centered_at(
        point(px(center.0), px(center.1)),
        Size::new(px(2.0 * radius), px(2.0 * radius)),
    )
}

fn paint_laser_dot(window: &mut Window, pos: (f32, f32), appearance: HighlightAppearance) {
    let color: Hsla = rgb(appearance.laser_color.packed()).into();
    let dot_radius = f32::from(appearance.laser_size);
    for (base, alpha) in DOT_GLOW {
        let radius = base * dot_radius / GLOW_BASE_RADIUS;
        window.paint_quad(fill(disc(pos, radius), color.opacity(alpha)).corner_radii(px(radius)));
    }
    window.paint_quad(fill(disc(pos, dot_radius), color).corner_radii(px(dot_radius)));
}

/// Per-layer alpha such that all [`FEATHER_LAYERS`] together composite to a
/// dimming of `dim_percent`.
#[expect(clippy::cast_precision_loss, reason = "a handful of feather layers")]
fn layer_alpha(dim_percent: u8) -> f32 {
    let dim = f32::from(dim_percent.min(100)) / 100.0;
    1.0 - (1.0 - dim).powf(1.0 / FEATHER_LAYERS as f32)
}

/// Dim everything outside a circle with one quad per feather layer.
///
/// The quad is centred on `pos` and sized `2 * (r + b)` with corner radius
/// `r + b`, a transparent fill and a border of width `b` — larger than the
/// display diagonal, so the quad's outer edge lies off-screen. The shader's
/// inner rounded rect (radius `r`) is then the only transparent part: the
/// hole. Layers at slightly larger radii stack their alpha outwards, which is
/// the soft edge.
fn paint_spotlight(
    window: &mut Window,
    pos: (f32, f32),
    size: Size<Pixels>,
    appearance: HighlightAppearance,
) {
    let spot_radius = f32::from(appearance.spotlight_size);
    // Far enough that even the farthest corner from `pos` is inside the border.
    let border = f32::from(size.width).hypot(f32::from(size.height)) + 2.0 * spot_radius;
    let alpha = layer_alpha(appearance.spotlight_dim);
    for layer in 0..FEATHER_LAYERS {
        #[expect(clippy::cast_precision_loss, reason = "a handful of feather layers")]
        let radius = spot_radius + layer as f32 * FEATHER_STEP;
        let outer = radius + border;
        window.paint_quad(quad(
            disc(pos, outer),
            px(outer),
            gpui::transparent_black(),
            px(border),
            gpui::black().opacity(alpha),
            gpui::BorderStyle::default(),
        ));
    }
}

/// Options for the transparent, unfocused window covering a display.
///
/// Windowed at the full display bounds rather than the `Maximized` covering
/// window the ring uses on Wayland: a maximized window stops below the macOS
/// menu bar, which would offset every press, and the highlight maps global
/// cursor positions onto the window.
fn window_options(
    display: Option<gpui::DisplayId>,
    bounds: Option<Bounds<Pixels>>,
) -> WindowOptions {
    WindowOptions {
        window_bounds: bounds.map(gpui::WindowBounds::Windowed),
        display_id: display,
        window_decorations: Some(gpui::WindowDecorations::Client),
        app_id: Some("openlogi-highlight".to_string()),
        ..ring_window_options()
    }
}

/// Where a press landed, in the local space of the display it was located
/// on; `None` when the press is on no known display.
fn anchor(located: &Located) -> Option<Anchor> {
    let ((point, bounds), display_origin) =
        located.point.zip(located.bounds).zip(located.origin)?;
    Some(Anchor {
        local: (
            f32::from(point.x - bounds.origin.x),
            f32::from(point.y - bounds.origin.y),
        ),
        display_origin,
    })
}

/// The persistent window and the display it covers.
struct Slot {
    handle: WindowHandle<HighlightView>,
    display: Option<gpui::DisplayId>,
}

/// Whether a window on `window_display`, currently `viewport` large, still
/// covers the display `target` a session starts on, so it can be reused
/// instead of recreated: the same display, at the size it is located at now (a
/// resolution or arrangement change keeps the id and moves the bounds). A
/// window or target without a known display never matches: its coverage
/// cannot be vouched for.
fn covers(
    window_display: Option<gpui::DisplayId>,
    viewport: Size<Pixels>,
    target: &Located,
) -> bool {
    window_display.is_some()
        && window_display == target.id
        && target.bounds.is_some_and(|bounds| bounds.size == viewport)
}

/// What an update asks of the window, given the session it shows.
#[derive(Debug, PartialEq)]
enum Step {
    /// The shown session ended for real: its final position is reported.
    Release,
    /// The agent was lost: the shown session is cleared, with nobody to report
    /// to and no release to speak of.
    Disconnect,
    /// The shown session continues.
    Advance(ActiveHighlight),
    /// A session the window does not show yet: whatever is shown is over, and
    /// the window is reused or recreated for this one.
    Begin(ActiveHighlight),
}

/// Decide what `update` asks for while the window shows session `shown`.
fn plan(shown: Option<u64>, update: HighlightUpdate) -> Step {
    match update {
        HighlightUpdate::Disconnected => Step::Disconnect,
        HighlightUpdate::Observed(HighlightObservation { active: None, .. }) => Step::Release,
        HighlightUpdate::Observed(HighlightObservation {
            active: Some(highlight),
            ..
        }) => {
            if shown == Some(highlight.session_id) {
                Step::Advance(highlight)
            } else {
                Step::Begin(highlight)
            }
        }
    }
}

/// What the presenter loop remembers between observations.
struct Showing {
    /// `None` until the first session opened one, and again after it vanished,
    /// failed its native policy or its display went away; the next session
    /// recreates it.
    window: Option<Slot>,
    /// Where a finished session's final position is reported.
    commands: mpsc::UnboundedSender<OverlayCommand>,
    /// Running position reports of the shown session.
    moves: MoveThrottle,
    /// A window opened inside a GPUI update whose native policy is still to
    /// be applied, outside it ([`configure_native`](Self::configure_native)).
    unconfigured: Option<(WindowHandle<HighlightView>, NativeView)>,
    /// The session no window could be had for: opening is attempted once per
    /// session, not again on each of its ~100 Hz observations.
    given_up: Option<u64>,
}

impl Showing {
    fn new(commands: mpsc::UnboundedSender<OverlayCommand>) -> Self {
        Self {
            window: None,
            commands,
            moves: MoveThrottle::default(),
            unconfigured: None,
            given_up: None,
        }
    }

    /// Remove the window, if any.
    fn close(&mut self, cx: &mut gpui::App) {
        self.unconfigured = None;
        if let Some(old) = self.window.take() {
            let _ = old.handle.update(cx, |_, window, _| window.remove_window());
        }
    }

    /// Replace the window with a fresh, idle one on `located`'s display. A
    /// failure is logged and leaves no window. A window whose native view is
    /// unknown cannot be made click-through, so it is not kept either.
    fn open(&mut self, cx: &mut gpui::App, located: &Located) {
        self.close(cx);
        let options = window_options(located.id, located.bounds);
        let handle =
            match cx.open_window(options, |_, cx| cx.new(|_| HighlightView { session: None })) {
                Ok(handle) => handle,
                Err(error) => {
                    warn!(%error, "could not open highlight window");
                    return;
                }
            };
        let Some(native) = handle
            .update(cx, |_, window, _| native_view(window))
            .ok()
            .flatten()
        else {
            warn!("highlight window has no native view to make click-through; closing it");
            let _ = handle.update(cx, |_, window, _| window.remove_window());
            return;
        };
        self.unconfigured = Some((handle, native));
        self.window = Some(Slot {
            handle,
            display: located.id,
        });
    }

    /// Apply the native window policy of a window opened by the last update.
    ///
    /// Its style and frame changes notify GPUI's resize callbacks, so this
    /// must run outside any app update, where they can borrow the app. Their
    /// resize handling is then resynced explicitly: the native frame was just
    /// corrected, and the viewport follows it. A window the policy could not
    /// be applied to is removed, never left swallowing clicks.
    fn configure_native(&mut self, cx: &mut gpui::AsyncApp) {
        let Some((handle, native)) = self.unconfigured.take() else {
            return;
        };
        let configured = crate::platform::configure_highlight_window(&native);
        cx.update(|cx| {
            if configured {
                let _ = handle.update(cx, |_, window, cx| window.bounds_changed(cx));
            } else {
                warn!("could not make the highlight window click-through; closing it");
                self.given_up = self.shown_session(cx);
                self.close(cx);
            }
        });
    }

    /// The id of the session the live window shows, if any.
    fn shown_session(&self, cx: &gpui::App) -> Option<u64> {
        let view = self.window.as_ref()?.handle.read(cx).ok()?;
        view.session.as_ref().map(|session| session.id)
    }

    /// Clear the shown session, if any, keeping the window, and return what
    /// to tell the agent about where its dot ended up.
    fn end_session(&mut self, cx: &mut gpui::App) -> Option<OverlayCommand> {
        // Nothing running is left to report a position for.
        self.moves.clear();
        let slot = self.window.as_ref()?;
        let released_at = Instant::now();
        let release = slot.handle.update(cx, |view, _, cx| {
            let session = view.session.take()?;
            // One repaint, now of nothing.
            cx.notify();
            session.release_command(released_at)
        });
        release.unwrap_or_else(|_| {
            // The window was closed behind our back.
            self.window = None;
            None
        })
    }

    /// The shown session, if any, ended for real: report its final position.
    fn end_released(&mut self, cx: &mut gpui::App) {
        if let Some(release) = self.end_session(cx) {
            let _ = self.commands.send(release);
        }
    }

    /// The agent was lost: clear the shown session, if any, without a report.
    fn end_disconnected(&mut self, cx: &mut gpui::App) {
        self.end_session(cx);
    }

    /// Fold a continuation into the shown session and report the new position
    /// when it moved. `false` when there is no live window or session.
    fn advance(&mut self, cx: &mut gpui::App, highlight: &ActiveHighlight) -> bool {
        let Some(slot) = &self.window else {
            return false;
        };
        let advanced = slot.handle.update(cx, |view, window, cx| {
            let size = window.viewport_size();
            let size = (f32::from(size.width), f32::from(size.height));
            let session = view.session.as_mut()?;
            let moved = session.advance(highlight, size, Instant::now());
            cx.notify();
            Some(moved.then(|| session.move_command()).flatten())
        });
        let Ok(Some(report)) = advanced else {
            return false;
        };
        if let Some(command) = report {
            self.report_move(command, Instant::now());
        }
        true
    }

    /// Show a new session, reusing the window when it still covers the
    /// press's display. Without a usable window the session is given up on.
    fn begin(&mut self, cx: &mut gpui::App, highlight: &ActiveHighlight) {
        let located = locate(cx, highlight.origin);
        let covered = self.window.as_ref().is_some_and(|slot| {
            slot.handle
                .update(cx, |_, window, _| window.viewport_size())
                .is_ok_and(|viewport| covers(slot.display, viewport, &located))
        });
        if !covered {
            self.open(cx, &located);
        }
        let begun = self.window.as_ref().map(|slot| {
            let anchor = anchor(&located);
            slot.handle.update(cx, |view, window, cx| {
                let size = window.viewport_size();
                let size = (f32::from(size.width), f32::from(size.height));
                view.session = Some(Session::new(highlight, anchor, size, Instant::now()));
                cx.notify();
            })
        });
        if !matches!(begun, Some(Ok(()))) {
            self.window = None;
            self.given_up = Some(highlight.session_id);
        }
    }

    /// Send `command` now or hold it for [`flush_moves`](Self::flush_moves),
    /// per the throttle.
    fn report_move(&mut self, command: OverlayCommand, now: Instant) {
        if let Some(command) = self.moves.offer(now, command) {
            let _ = self.commands.send(command);
        }
    }

    /// How long until a held position report may go out.
    fn move_wait(&self) -> Option<Duration> {
        self.moves.wait(Instant::now())
    }

    /// Send the held position report, if its time has come.
    fn flush_moves(&mut self) {
        if let Some(command) = self.moves.flush(Instant::now()) {
            let _ = self.commands.send(command);
        }
    }

    fn apply(&mut self, cx: &mut gpui::App, update: HighlightUpdate) {
        if !HIGHLIGHT_OVERLAY_SUPPORTED {
            return;
        }
        match plan(self.shown_session(cx), update) {
            Step::Release => {
                self.given_up = None;
                self.end_released(cx);
            }
            Step::Disconnect => {
                self.given_up = None;
                self.end_disconnected(cx);
            }
            Step::Advance(highlight) | Step::Begin(highlight)
                if self.given_up == Some(highlight.session_id) => {}
            Step::Advance(highlight) => {
                // The window can vanish between the plan and the update.
                if !self.advance(cx, &highlight) {
                    self.begin(cx, &highlight);
                }
            }
            Step::Begin(highlight) => {
                // The previous session (if any) is over.
                self.end_released(cx);
                self.begin(cx, &highlight);
            }
        }
    }
}

/// Collapse a burst of updates to the ones that matter. Motion arrives at
/// ~100 Hz and travel is cumulative, so only the newest update matters; a
/// session change in between simply shows the newer session, and a disconnect
/// in between ends it. The exception is a burst whose newest update ends the
/// session: the last skipped active observation is kept before it, or the
/// travel it carried would never reach the session and the release would
/// omit the final motion.
fn coalesce(burst: impl IntoIterator<Item = HighlightUpdate>) -> Vec<HighlightUpdate> {
    let mut newest = None;
    let mut last_active = None;
    for update in burst {
        if let Some(skipped) = newest.replace(update) {
            last_active = matches!(
                skipped,
                HighlightUpdate::Observed(HighlightObservation {
                    active: Some(_),
                    ..
                })
            )
            .then_some(skipped);
        }
    }
    let Some(newest) = newest else {
        return Vec::new();
    };
    match newest {
        HighlightUpdate::Observed(HighlightObservation { active: None, .. }) => {
            last_active.into_iter().chain([newest]).collect()
        }
        _ => vec![newest],
    }
}

/// Drive the highlight window from the agent's observations for the life of
/// the app.
pub(crate) fn spawn(
    cx: &mut gpui::App,
    mut highlights: mpsc::UnboundedReceiver<HighlightUpdate>,
    commands: mpsc::UnboundedSender<OverlayCommand>,
) {
    let mut showing = Showing::new(commands);
    cx.spawn(async move |cx| {
        loop {
            // A held position report goes out when its interval is up, even
            // if the device is quiet by then.
            let next = match showing.move_wait() {
                Some(wait) => {
                    tokio::select! {
                        next = highlights.recv() => next,
                        () = cx.background_executor().timer(wait) => {
                            showing.flush_moves();
                            continue;
                        }
                    }
                }
                None => highlights.recv().await,
            };
            let Some(first) = next else {
                break;
            };
            let mut burst = vec![first];
            while let Ok(newer) = highlights.try_recv() {
                burst.push(newer);
            }
            for update in coalesce(burst) {
                cx.update(|cx| showing.apply(cx, update));
                showing.configure_native(cx);
            }
        }
    })
    .detach();
}

#[cfg(test)]
mod tests;
