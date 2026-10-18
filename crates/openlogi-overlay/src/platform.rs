//! Native window policy for the overlay's windows: the Actions Ring panel and
//! the click-through presenter highlight.
//!
//! macOS applies it through AppKit and CoreGraphics (`macos.rs`); every other
//! OS implements the same functions as documented no-ops (`other.rs`). This
//! facade owns the types both share and picks one of the two.

mod display;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(not(target_os = "macos"))]
mod other;
#[cfg(not(target_os = "windows"))]
mod placement;
// Keep Windows geometry tests runnable on the host; only native.rs needs Win32.
#[cfg(any(target_os = "windows", test))]
mod windows;

#[cfg(target_os = "macos")]
use macos as os;
#[cfg(not(target_os = "macos"))]
use other as os;

pub(crate) use display::{Located, locate};
pub use os::*;
#[cfg(not(target_os = "windows"))]
pub(crate) use placement::RingPlacement;
#[cfg(target_os = "windows")]
pub(crate) use windows::RingPlacement;

/// One display's global geometry, in the same top-left-origin global point
/// space that `openlogi_hook::cursor_position()` reports.
pub struct CursorDisplay {
    /// Native display id; on macOS the `CGDirectDisplayID`, numerically equal
    /// to GPUI's `DisplayId` for the same display.
    pub id: u64,
    /// Global origin (top-left corner) of the display, in points.
    pub origin: (f64, f64),
    /// Display size in points.
    pub size: (f64, f64),
}
