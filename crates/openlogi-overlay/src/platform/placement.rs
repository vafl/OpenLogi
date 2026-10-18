//! GPUI placement on macOS and Linux.

use gpui::{Bounds, PlatformDisplay, Size, WindowBounds, WindowOptions, point, px};
use tracing::warn;

use crate::ring::{Placement, WINDOW_SIZE, clamp_window_origin, ring_window_options};

/// Captured placement for one invocation, before its windows are opened.
pub(crate) struct RingPlacement {
    windows: Vec<(WindowOptions, Placement)>,
}

impl RingPlacement {
    #[expect(
        clippy::unnecessary_wraps,
        reason = "the Windows placement capture is fallible"
    )]
    pub(crate) fn capture(cx: &mut gpui::App) -> anyhow::Result<Self> {
        if on_wayland() {
            let displays = cx.displays();
            if !displays.is_empty() {
                return Ok(Self {
                    windows: displays
                        .iter()
                        .map(|display| {
                            (
                                covering_window_options(display.as_ref()),
                                Placement::AtPointer { center: None },
                            )
                        })
                        .collect(),
                });
            }
            warn!("no displays reported; falling back to a positioned Actions Ring window");
        }
        Ok(Self {
            windows: vec![positioned_window(cx)],
        })
    }

    /// Window options for this invocation, paired with how each view places its panel.
    pub(crate) fn openings(&mut self) -> Vec<(WindowOptions, Placement)> {
        std::mem::take(&mut self.windows)
    }

    #[expect(
        clippy::unnecessary_wraps,
        clippy::unused_self,
        reason = "the Windows implementation consumes the captured geometry and can fail"
    )]
    pub(crate) fn show(self, _window: &mut gpui::Window) -> anyhow::Result<()> {
        super::configure_windows();
        Ok(())
    }
}

fn positioned_window(cx: &mut gpui::App) -> (WindowOptions, Placement) {
    let cursor = openlogi_hook::cursor_position().map(|cursor| (cursor.x, cursor.y));
    let located = super::locate(cx, cursor);
    let size = Size::new(px(WINDOW_SIZE), px(WINDOW_SIZE));
    let center = located
        .point
        .or_else(|| located.bounds.map(|bounds| bounds.center()))
        .unwrap_or_default();
    let desired_origin = point(center.x - size.width / 2.0, center.y - size.height / 2.0);
    let origin = located.bounds.map_or(desired_origin, |display_bounds| {
        clamp_window_origin(desired_origin, size, display_bounds)
    });
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::new(origin, size))),
        display_id: located.id,
        ..ring_window_options()
    };
    (options, Placement::Window)
}

/// Transparent maximized window covering `display`; fullscreen would composite it against black.
fn covering_window_options(display: &dyn PlatformDisplay) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Maximized(display.bounds())),
        display_id: Some(display.id()),
        window_decorations: Some(gpui::WindowDecorations::Client),
        ..ring_window_options()
    }
}

/// Report whether GPUI's own backend selection is Wayland.
fn on_wayland() -> bool {
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    {
        gpui::guess_compositor() == "Wayland"
    }
    #[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
    {
        false
    }
}
