//! Native window policy for the overlay's windows: the Actions Ring panel and
//! the click-through presenter highlight.

mod display;
#[cfg(not(target_os = "windows"))]
mod placement;
// Keep Windows geometry tests runnable on the host; only native.rs needs Win32.
#[cfg(any(target_os = "windows", test))]
mod windows;

pub(crate) use display::{Located, locate};
#[cfg(not(target_os = "windows"))]
pub(crate) use placement::RingPlacement;
#[cfg(target_os = "windows")]
pub(crate) use windows::RingPlacement;

/// Keep the overlay out of the Dock and app switcher.
#[cfg(target_os = "macos")]
pub fn configure_application() {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};

    if let Some(marker) = MainThreadMarker::new() {
        NSApplication::sharedApplication(marker)
            .setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    }
}

/// Make the transparent ring panel borderless and remove its native shadow.
///
/// Runs inside a GPUI update, where a style change on the persistent
/// highlight window would notify GPUI's resize callbacks while the app is
/// borrowed; the highlight window is recognised by the click-through its own
/// policy set and left alone.
#[cfg(target_os = "macos")]
pub fn configure_windows() {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSWindowStyleMask};

    if let Some(marker) = MainThreadMarker::new() {
        for window in NSApplication::sharedApplication(marker)
            .windows()
            .into_iter()
            .filter(|window| !window.ignoresMouseEvents())
        {
            window.setStyleMask(NSWindowStyleMask::NonactivatingPanel);
            window.setHasShadow(false);
        }
    }
}

/// Identity of the native view GPUI renders a window into, taken while the
/// window is borrowed so the window can be configured natively later, outside
/// any GPUI update. Only compared, never dereferenced.
#[cfg(target_os = "macos")]
pub struct NativeView(std::ptr::NonNull<std::ffi::c_void>);

/// No native window policy is applied away from macOS.
#[cfg(not(target_os = "macos"))]
pub struct NativeView(());

/// The native view of GPUI `window`, if it has one.
#[cfg(target_os = "macos")]
pub fn native_view(window: &gpui::Window) -> Option<NativeView> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let handle = HasWindowHandle::window_handle(window).ok()?;
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return None;
    };
    Some(NativeView(handle.ns_view))
}

/// No native view away from macOS: nothing here needs one.
#[cfg(not(target_os = "macos"))]
pub fn native_view(_window: &gpui::Window) -> Option<NativeView> {
    None
}

/// The `NSWindow` hosting `view`, if it can be found.
///
/// GPUI's raw handle is its own view, a subview of the window's content
/// view. Match pointers against each NSWindow's view tree instead of
/// dereferencing the raw pointer: no unsafe, and a stale handle simply
/// matches nothing.
#[cfg(target_os = "macos")]
fn native_window(
    view: &NativeView,
    marker: objc2::MainThreadMarker,
) -> Option<objc2::rc::Retained<objc2_app_kit::NSWindow>> {
    use objc2_app_kit::NSApplication;

    NSApplication::sharedApplication(marker)
        .windows()
        .into_iter()
        .find(|native| {
            native.contentView().is_some_and(|content| {
                content.subviews().iter().any(|subview| {
                    objc2::rc::Retained::as_ptr(&subview).cast::<std::ffi::c_void>()
                        == view.0.as_ptr()
                })
            })
        })
}

/// Make a highlight window click-through, shadowless, and high enough to stay
/// above full-screen presentation apps.
///
/// GPUI places a `PopUp` at `NSPopUpWindowLevel` (101), which a full-screen
/// slideshow's own space can still cover. The highlight is raised to
/// `NSScreenSaverWindowLevel` (1000) and joins every space as a full-screen
/// auxiliary window so it draws over a presenter's full-screen app. Applied to
/// highlight windows only: the ring must keep receiving mouse events.
///
/// Style and frame changes make AppKit notify GPUI's resize callbacks, which
/// borrow the app: call this outside any GPUI update (with the view taken
/// from [`native_view`] inside one), or each change logs "RefCell already
/// borrowed" and GPUI misses the new size.
///
/// Returns whether the policy was applied. A highlight window is persistent
/// and covers its display, so one that is not click-through would swallow
/// every click: on `false` the caller must remove it.
#[cfg(target_os = "macos")]
#[must_use]
pub fn configure_highlight_window(view: &NativeView) -> bool {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSScreenSaverWindowLevel, NSWindowCollectionBehavior, NSWindowStyleMask};

    let Some(marker) = MainThreadMarker::new() else {
        return false;
    };
    let Some(native) = native_window(view, marker) else {
        return false;
    };
    // First, so a failure past this point leaves the window click-through.
    native.setIgnoresMouseEvents(true);
    native.setStyleMask(NSWindowStyleMask::NonactivatingPanel);
    native.setHasShadow(false);
    // AppKit keeps a window below the menu bar when it is shown, which
    // shifts it by the bar's height; the highlight maps global cursor
    // positions onto the window, so it must sit exactly on its screen.
    if let Some(screen) = native.screen() {
        native.setFrame_display(screen.frame(), true);
    }
    native.setLevel(NSScreenSaverWindowLevel);
    native.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::FullScreenAuxiliary
            | NSWindowCollectionBehavior::Stationary
            | NSWindowCollectionBehavior::IgnoresCycle,
    );
    true
}

/// The click-through and level policy is macOS-only, and so is the highlight
/// window that needs it (`HIGHLIGHT_OVERLAY_SUPPORTED`); nothing opens one
/// elsewhere, so there is nothing to configure and nothing to fail.
#[cfg(not(target_os = "macos"))]
#[must_use]
pub fn configure_highlight_window(_view: &NativeView) -> bool {
    true
}

/// No native application policy is required away from macOS.
#[cfg(not(target_os = "macos"))]
pub fn configure_application() {}

/// Linux needs no additional native window configuration here.
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn configure_windows() {}

/// Owner of the native click-away event monitor; dropping it removes the
/// monitor. Create and drop on the main thread.
#[cfg(target_os = "macos")]
pub struct ClickAwayMonitor(objc2::rc::Retained<objc2::runtime::AnyObject>);

#[cfg(target_os = "macos")]
impl Drop for ClickAwayMonitor {
    #[expect(
        unsafe_code,
        reason = "NSEvent::removeMonitor is plain AppKit FFI; the token is exactly what addGlobalMonitor returned"
    )]
    fn drop(&mut self) {
        // SAFETY: `self.0` is the monitor token returned by
        // `addGlobalMonitorForEventsMatchingMask_handler`, removed only once.
        unsafe { objc2_app_kit::NSEvent::removeMonitor(&self.0) };
    }
}

/// Invoke `on_mouse_down` for every mouse-down that macOS delivers to *other*
/// applications, for as long as the returned monitor is held.
///
/// Global `NSEvent` monitors never see events routed to this process's own
/// windows and cannot consume the events they observe — together exactly the
/// ring's click-away contract: clicks on the ring keep hitting the GPUI
/// handlers they always did, while a click anywhere else can dismiss the ring
/// without being swallowed. Must be called on the main thread (returns `None`
/// off it); the handler runs on the main run loop.
#[cfg(target_os = "macos")]
pub fn watch_clicks_outside(on_mouse_down: impl Fn() + 'static) -> Option<ClickAwayMonitor> {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSEvent, NSEventMask};

    MainThreadMarker::new()?;
    let handler: block2::RcBlock<dyn Fn(std::ptr::NonNull<NSEvent>)> =
        block2::RcBlock::new(move |_event| on_mouse_down());
    NSEvent::addGlobalMonitorForEventsMatchingMask_handler(
        NSEventMask::LeftMouseDown | NSEventMask::RightMouseDown | NSEventMask::OtherMouseDown,
        &handler,
    )
    .map(ClickAwayMonitor)
}

/// Away from macOS no global click monitor is available; the ring keeps its
/// in-window dismissal paths (center ×, slot activation, timeout).
#[cfg(not(target_os = "macos"))]
pub struct ClickAwayMonitor(());

#[cfg(not(target_os = "macos"))]
pub fn watch_clicks_outside(_on_mouse_down: impl Fn() + 'static) -> Option<ClickAwayMonitor> {
    None
}

/// One display's global geometry, in the same top-left-origin global point
/// space that `openlogi_hook::cursor_position()` reports.
#[cfg(not(target_os = "windows"))]
pub struct CursorDisplay {
    /// Native display id; on macOS the `CGDirectDisplayID`, numerically equal
    /// to GPUI's `DisplayId` for the same display.
    pub id: u64,
    /// Global origin (top-left corner) of the display, in points.
    pub origin: (f64, f64),
    /// Display size in points.
    pub size: (f64, f64),
}

/// Find the display whose global bounds contain the point `(x, y)`.
///
/// GPUI's `PlatformDisplay::bounds()` zeroes every display's origin (window
/// bounds are display-relative), so mapping a global cursor position to its
/// display has to go through CoreGraphics.
#[cfg(target_os = "macos")]
#[expect(
    unsafe_code,
    reason = "CGGetActiveDisplayList/CGDisplayBounds are plain C FFI; GPUI exposes no global display bounds"
)]
pub fn display_containing(x: f64, y: f64) -> Option<CursorDisplay> {
    use core_graphics::display::{CGDisplayBounds, CGGetActiveDisplayList};

    const MAX_DISPLAYS: u32 = 32;
    let mut ids = [0u32; MAX_DISPLAYS as usize];
    let mut count = 0u32;
    // SAFETY: the list write is bounded by the capacity we pass; `count`
    // reports how many entries were actually filled.
    let result = unsafe { CGGetActiveDisplayList(MAX_DISPLAYS, ids.as_mut_ptr(), &raw mut count) };
    if result != 0 {
        return None;
    }
    ids.iter().take(count as usize).find_map(|&id| {
        // SAFETY: side-effect-free C getter on an id from the active list.
        let bounds = unsafe { CGDisplayBounds(id) };
        let contains = x >= bounds.origin.x
            && x < bounds.origin.x + bounds.size.width
            && y >= bounds.origin.y
            && y < bounds.origin.y + bounds.size.height;
        contains.then(|| CursorDisplay {
            id: u64::from(id),
            origin: (bounds.origin.x, bounds.origin.y),
            size: (bounds.size.width, bounds.size.height),
        })
    })
}

/// On Linux the GPUI display list already carries global origins, so there is
/// nothing to resolve natively.
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn display_containing(_x: f64, _y: f64) -> Option<CursorDisplay> {
    None
}
