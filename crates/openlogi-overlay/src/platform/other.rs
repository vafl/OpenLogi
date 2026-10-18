//! The overlay's native window policy on every OS but macOS: there is none
//! to apply, so each function `macos.rs` implements is a documented no-op
//! here.

use super::CursorDisplay;

/// No native application policy is required away from macOS.
pub fn configure_application() {}

/// Linux needs no additional native window configuration here. Windows
/// places its ring through Win32 instead (`windows/native.rs`) and never
/// calls this.
#[cfg(not(target_os = "windows"))]
pub fn configure_windows() {}

/// No native window policy is applied away from macOS.
pub struct NativeView(());

/// No native view away from macOS: nothing here needs one.
pub fn native_view(_window: &gpui::Window) -> Option<NativeView> {
    None
}

/// The click-through and level policy is macOS-only, and so is the highlight
/// window that needs it (`HIGHLIGHT_OVERLAY_SUPPORTED`); nothing opens one
/// elsewhere, so there is nothing to configure and nothing to fail.
#[must_use]
pub fn configure_highlight_window(_view: &NativeView) -> bool {
    true
}

/// Away from macOS no global click monitor is available; the ring keeps its
/// in-window dismissal paths (center ×, slot activation, timeout).
pub struct ClickAwayMonitor(());

/// No global click monitor away from macOS: always `None`.
pub fn watch_clicks_outside(_on_mouse_down: impl Fn() + 'static) -> Option<ClickAwayMonitor> {
    None
}

/// GPUI's display list already carries global origins on Linux and Windows,
/// so there is nothing to resolve natively.
pub fn display_containing(_x: f64, _y: f64) -> Option<CursorDisplay> {
    None
}
