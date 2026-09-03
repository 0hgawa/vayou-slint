//! Unix implementation of the window chrome described in the parent module.
//!
//! Where the Windows side reaches for Win32, this asks winit — which already
//! owns the frameless window, hands the move loop to the compositor and
//! delivers file drops as ordinary events. That is why this file is a fraction
//! of its Win32 counterpart: most of what Win32 needed by hand is one call here.
//!
//! Window state (fullscreen, maximize, minimize) goes through Slint's own API,
//! which covers all three, so winit is reached for only what Slint does not
//! expose: file drops, window level, the title-bar drag and cursor visibility.
//!
//! Every entry point is a no-op before the native window exists, which winit
//! reports by handing back `None`.

use std::cell::RefCell;

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use slint::winit_030::{winit, EventResult, WinitWindowAccessor};
use winit::window::WindowLevel;

thread_local! {
    static DROP_HANDLER: RefCell<Option<Box<dyn Fn(String)>>> = const { RefCell::new(None) };
}

/// Register a callback for files dropped onto the window (set from `main`).
pub fn set_drop_handler(f: impl Fn(String) + 'static) {
    DROP_HANDLER.with(|h| *h.borrow_mut() = Some(Box::new(f)));
}

/// Whether this window is a Wayland surface rather than an X11 one.
///
/// It decides one thing: whether a raise request can be honoured. Wayland gives
/// a client no way to place itself above others — stacking belongs to the
/// compositor — so `set_window_level` is silently ignored there. Asking the
/// handle is exact, and beats reading `WAYLAND_DISPLAY`, which says what the
/// session offers rather than what this window actually got.
fn is_wayland(win: &slint::Window) -> bool {
    let slint_handle = win.window_handle();
    slint_handle
        .window_handle()
        .is_ok_and(|h| matches!(h.as_raw(), RawWindowHandle::Wayland(_)))
}

/// Subscribe to the window's winit events so a dropped file reaches the handler.
/// Returns false while the native window does not exist yet, so the caller keeps
/// trying.
///
/// Nothing else is hooked here: the frame, the icon and the drag loop already
/// belong to winit and the compositor.
pub fn attach_ui(ui: &slint::Window) -> bool {
    if !ui.has_winit_window() {
        return false;
    }
    ui.on_winit_window_event(|_win, event| {
        if let winit::event::WindowEvent::DroppedFile(path) = event {
            let path = path.to_string_lossy().into_owned();
            DROP_HANDLER.with(|h| {
                if let Some(f) = h.borrow().as_ref() {
                    f(path);
                }
            });
        }
        EventResult::Propagate
    });
    true
}

/// Toggle borderless fullscreen. Returns the new state.
pub fn toggle_fullscreen(win: &slint::Window) -> bool {
    let on = !win.is_fullscreen();
    win.set_fullscreen(on);
    on
}

/// Toggle maximize. No-op while fullscreen, matching the Windows behaviour —
/// otherwise leaving fullscreen would restore to a state the user never chose.
pub fn toggle_maximize(win: &slint::Window) {
    if win.is_fullscreen() {
        return;
    }
    win.set_maximized(!win.is_maximized());
}

/// Pin the window above the others, reporting whether that actually happened.
///
/// On X11 it does. On Wayland no protocol lets a client raise itself, so the
/// request is refused up front rather than being passed to a call that would
/// swallow it — the caller needs the truth to avoid showing a toggle that is on
/// while nothing changed.
pub fn set_always_on_top(win: &slint::Window, enabled: bool) -> bool {
    if is_wayland(win) {
        return false;
    }
    let level = if enabled { WindowLevel::AlwaysOnTop } else { WindowLevel::Normal };
    win.with_winit_window(|w| w.set_window_level(level)).is_some()
}

/// Begin a window move from the title bar. winit hands this to the compositor,
/// which runs its own move loop — so unlike the Win32 path there is no stuck
/// mouse-button state to repair afterwards.
pub fn start_drag(win: &slint::Window) {
    if win.is_fullscreen() || win.is_maximized() {
        return;
    }
    // A compositor may refuse the request (no active pointer grab, say). That is
    // the drag not starting, which is already what the user sees; there is
    // nothing to recover.
    let _ = win.with_winit_window(|w| w.drag_window().is_ok());
}

pub fn minimize(win: &slint::Window) {
    win.set_minimized(true);
}

/// Show or hide the pointer over the window (used by the fullscreen idle
/// auto-hide). Unlike Slint's reactive `mouse-cursor`, this takes effect
/// immediately even while the pointer is stationary.
pub fn set_cursor_hidden(win: &slint::Window, hidden: bool) {
    let _ = win.with_winit_window(|w| w.set_cursor_visible(!hidden));
}
