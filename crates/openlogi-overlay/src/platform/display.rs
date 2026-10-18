//! Which display a global point is on, for the windows that follow the
//! pointer: the Actions Ring panel and the presenter highlight.

use gpui::{Bounds, DisplayId, PlatformDisplay, Point, point, px};

/// A global point resolved to its display. GPUI's display-relative and
/// global spaces differ per platform, so the point and bounds are given in
/// the one space window bounds are expressed in, and the display's global
/// origin is kept apart.
pub(crate) struct Located {
    /// The display; `None` when GPUI reports no displays.
    pub(crate) id: Option<DisplayId>,
    /// The display's bounds in window space.
    pub(crate) bounds: Option<Bounds<gpui::Pixels>>,
    /// The queried point in window space; `None` when no point was given.
    pub(crate) point: Option<Point<gpui::Pixels>>,
    /// Global top-left-origin position of the display's top-left corner;
    /// `None` when the point is on no known display (it fell back to the
    /// primary display, or none).
    pub(crate) origin: Option<(f64, f64)>,
}

impl Located {
    fn on(display: Option<&dyn PlatformDisplay>, point: Option<Point<gpui::Pixels>>) -> Self {
        Self {
            id: display.map(PlatformDisplay::id),
            bounds: display.map(PlatformDisplay::bounds),
            point,
            origin: None,
        }
    }
}

/// Resolve `global`, a point in `openlogi_hook::cursor_position()`'s space,
/// to the display holding it, falling back to the primary display.
#[expect(
    clippy::cast_possible_truncation,
    reason = "native coordinates are screen-sized and exactly usable as GPUI f32 pixels"
)]
pub(crate) fn locate(cx: &gpui::App, global: Option<(f64, f64)>) -> Located {
    let Some((x, y)) = global else {
        return Located::on(cx.primary_display().as_deref(), None);
    };
    // macOS GPUI bounds are display-relative; translate the global point
    // into the native display's space. Elsewhere there is no native lookup.
    if let Some(display) = super::display_containing(x, y) {
        return Located {
            id: Some(DisplayId::from(display.id)),
            bounds: Some(Bounds::new(
                Point::default(),
                gpui::Size::new(px(display.size.0 as f32), px(display.size.1 as f32)),
            )),
            point: Some(point(
                px((x - display.origin.0) as f32),
                px((y - display.origin.1) as f32),
            )),
            origin: Some(display.origin),
        };
    }
    // Linux, Windows, or a failed macOS lookup: displays carry global bounds.
    let global = point(px(x as f32), px(y as f32));
    let display = cx
        .displays()
        .into_iter()
        .find(|display| display.bounds().contains(&global));
    match display {
        Some(display) => {
            let bounds = display.bounds();
            Located {
                origin: Some((
                    f64::from(f32::from(bounds.origin.x)),
                    f64::from(f32::from(bounds.origin.y)),
                )),
                ..Located::on(Some(display.as_ref()), Some(global))
            }
        }
        None => Located::on(cx.primary_display().as_deref(), Some(global)),
    }
}
