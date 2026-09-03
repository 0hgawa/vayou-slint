//! Window-chrome callbacks: fullscreen, always-on-top, minimize / maximize /
//! close, and title-bar drag. Thin wrappers over `crate::win`.

use std::time::Duration;

use slint::ComponentHandle;

use crate::error::LogErr;
use crate::win;
use crate::MainWindow;

/// How long the window stays gone before the process quits.
///
/// This is not a wait anyone sits through — the window is already off screen.
/// It is what buys the shell its close animation, which needs the surface
/// destroyed *and* the client still connected to play; quitting immediately
/// pulled the connection out from under it and the window just vanished.
/// GNOME's runs for about 200 ms.
const COMPOSITOR_ANIMATION: Duration = Duration::from_millis(250);

/// Take the window off screen now; quit once the shell has had its moment.
///
/// The order is the whole point. Dying takes this process the better part of a
/// second — 4K buffers, the nvdec context, the driver's share of both — and
/// under Wayland the surface stays on screen for every bit of that, because it
/// only goes when the connection does. Hiding first spends that second with
/// nobody watching.
///
/// The render context is abandoned rather than freed because freeing it *is*
/// most of that second, and `hide()` does it before dropping the surface:
/// measured at 391 ms of frozen window, against 0.7 ms when it is skipped. That
/// leaks nothing the kernel will not reclaim — this process exits without
/// unwinding regardless, so the mpv core is never destroyed either, which is the
/// one thing libmpv forbids while a render context is alive. The context was in
/// fact never freed before this function existed; the difference is that now it
/// is deliberate.
pub(crate) fn close_app(ui: &MainWindow) {
    crate::video_render::abandon_context_on_teardown();
    ui.window().hide().log_err("hide window on close");
    slint::Timer::single_shot(COMPOSITOR_ANIMATION, || {
        let _ = slint::quit_event_loop();
    });
}

/// Toggle borderless fullscreen and reflect the new state in the UI.
pub(crate) fn toggle_fullscreen(ui: &MainWindow) {
    ui.set_fullscreen(win::toggle_fullscreen(ui.window()));
}

pub(crate) fn wire(ui: &MainWindow) {
    ui.on_toggle_fullscreen({
        let ui_w = ui.as_weak();
        move || { if let Some(ui) = ui_w.upgrade() { toggle_fullscreen(&ui); } }
    });
    ui.on_set_always_on_top({
        let ui_w = ui.as_weak();
        move |on| {
            let Some(ui) = ui_w.upgrade() else { return };
            // Show what happened, not what was asked for. A Wayland client
            // cannot raise itself — stacking belongs to the compositor — so the
            // toggle has to fall back rather than sit lit over a window that
            // never moved.
            let applied = win::set_always_on_top(ui.window(), on);
            ui.set_pinned(on && applied);
            if on && !applied {
                ui.set_toast("Always on top isn't available on this desktop".into());
            }
        }
    });
    ui.on_win_minimize({
        let ui_w = ui.as_weak();
        move || { if let Some(ui) = ui_w.upgrade() { win::minimize(ui.window()); } }
    });
    ui.on_win_maximize({
        let ui_w = ui.as_weak();
        move || { if let Some(ui) = ui_w.upgrade() { win::toggle_maximize(ui.window()); } }
    });
    ui.on_win_close({
        let ui_w = ui.as_weak();
        move || { if let Some(ui) = ui_w.upgrade() { close_app(&ui); } }
    });
    ui.on_start_window_drag({
        let ui_w = ui.as_weak();
        move || { if let Some(ui) = ui_w.upgrade() { win::start_drag(ui.window()); } }
    });
    ui.on_set_os_cursor_hidden({
        let ui_w = ui.as_weak();
        move |hidden| { if let Some(ui) = ui_w.upgrade() { win::set_cursor_hidden(ui.window(), hidden); } }
    });
    // Closing via the OS (Alt+F4, taskbar menu, etc.) must quit the loop too.
    // With `run_event_loop_until_quit`, an unhandled close only *hides* the
    // window — the process and mpv's audio would keep running in the background,
    // and reopening would stack zombie instances that fight over input/audio.
    ui.window().on_close_requested({
        let ui_w = ui.as_weak();
        move || {
            if let Some(ui) = ui_w.upgrade() { close_app(&ui); }
            slint::CloseRequestResponse::HideWindow
        }
    });
}
