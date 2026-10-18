//! Hiding and showing the mouse pointer from a background process.
//!
//! macOS only honours cursor changes (`NSCursor`, `CGDisplayHideCursor`) from
//! the application that is active, and the agent is never active. The
//! WindowServer connection property `SetsCursorInBackground` lifts that
//! restriction for one connection, but there is no public API to set it. The
//! two `CGS*` connection calls are private and in no binding crate, so they
//! are resolved lazily with `dlsym` like the crate's other private SPIs
//! (`app_services.rs`): a macOS release that drops them degrades to a single
//! warning and a pointer that stays visible, instead of a binary that fails to
//! load. Other cursor-hiding utilities set the same property. The hide itself
//! uses the public `CGDisplayHideCursor` / `CGDisplayShowCursor` through
//! `core-graphics`.
//!
//! The WindowServer keeps a *counter* of hides, so a second hide needs a
//! second show. `set_hidden` tracks one flag instead, which keeps the count
//! at zero or one however often it is called. A crashed agent needs no
//! cleanup: the WindowServer resets per-connection cursor state with the
//! connection.

use std::ffi::{c_int, c_void};
use std::sync::{Mutex, Once, PoisonError};

use core_foundation::base::TCFType;
use core_foundation::boolean::CFBoolean;
use core_foundation::string::CFString;
use core_graphics::display::CGDisplay;

use super::app_services::core_graphics_symbol;

// The signatures are those of the long-known CGS declarations (`CGSConnection.h`
// in the community-maintained CGSInternal headers, which every utility using
// these SPIs follows): `CGSConnectionID CGSMainConnectionID(void)` and
// `CGError CGSSetConnectionProperty(CGSConnectionID cid, CGSConnectionID
// targetCID, CFStringRef key, CFTypeRef value)`, a `CGSConnectionID` being an
// `int` and a `CGError` a 32-bit status.
type CGSConnectionID = c_int;
type MainConnectionIdFn = unsafe extern "C" fn() -> CGSConnectionID;
type SetConnectionPropertyFn = unsafe extern "C" fn(
    cid: CGSConnectionID,
    target_cid: CGSConnectionID,
    key: *const c_void,
    value: *const c_void,
) -> c_int;

/// Whether this process currently holds one hide on the cursor.
static HIDDEN: Mutex<bool> = Mutex::new(false);
static ENABLE_BACKGROUND: Once = Once::new();

/// Resolve the two connection calls from CoreGraphics; `None` if either is
/// unavailable on this macOS version.
fn connection_api() -> Option<(MainConnectionIdFn, SetConnectionPropertyFn)> {
    let main_connection_id = core_graphics_symbol(c"CGSMainConnectionID")?;
    let set_property = core_graphics_symbol(c"CGSSetConnectionProperty")?;
    // SAFETY: `CGSMainConnectionID`, when present, has the signature of
    // `MainConnectionIdFn`.
    let main_connection_id =
        unsafe { std::mem::transmute::<*mut c_void, MainConnectionIdFn>(main_connection_id) };
    // SAFETY: `CGSSetConnectionProperty`, when present, has the signature of
    // `SetConnectionPropertyFn`.
    let set_property =
        unsafe { std::mem::transmute::<*mut c_void, SetConnectionPropertyFn>(set_property) };
    Some((main_connection_id, set_property))
}

/// Allow this process to change the cursor while inactive. Once per process;
/// without the SPI the pointer simply stays visible. The property is never
/// reset: it belongs to this process's WindowServer connection and dies with
/// it, so there is nothing to undo at exit.
fn enable_background_cursor() {
    ENABLE_BACKGROUND.call_once(|| {
        let Some((main_connection_id, set_property)) = connection_api() else {
            tracing::warn!("CGS connection API unavailable; the pointer stays visible");
            return;
        };
        let key = CFString::from_static_string("SetsCursorInBackground");
        let value = CFBoolean::true_value();
        // SAFETY: the resolved `CGSMainConnectionID` takes no arguments and
        // only reads this process's WindowServer connection.
        let cid = unsafe { main_connection_id() };
        // SAFETY: the resolved `CGSSetConnectionProperty` is called with this
        // process's own connection as both ids, and `key` and `value` are
        // live CF objects for the call; the WindowServer copies the property
        // rather than retaining our pointers.
        let status = unsafe {
            set_property(
                cid,
                cid,
                key.as_concrete_TypeRef().cast(),
                value.as_concrete_TypeRef().cast(),
            )
        };
        if status != 0 {
            tracing::warn!(status, "SetsCursorInBackground was refused");
        }
    });
}

/// Hide the pointer; a repeat does nothing, so hides never nest.
pub(in crate::inject) fn hide() {
    set_hidden(true);
}

/// Show the pointer; a repeat does nothing.
pub(in crate::inject) fn show() {
    set_hidden(false);
}

fn set_hidden(hidden: bool) {
    let mut current = HIDDEN.lock().unwrap_or_else(PoisonError::into_inner);
    if *current == hidden {
        return;
    }
    if hidden {
        enable_background_cursor();
    }
    let display = CGDisplay::main();
    let result = if hidden {
        display.hide_cursor()
    } else {
        display.show_cursor()
    };
    match result {
        Ok(()) => *current = hidden,
        Err(error) => tracing::warn!(error, hidden, "changing the pointer visibility failed"),
    }
}
