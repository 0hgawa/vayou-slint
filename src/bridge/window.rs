//! Window-chrome callbacks: fullscreen, always-on-top, minimize / maximize /
//! close, and title-bar drag. Thin wrappers over `crate::win`.

use slint::ComponentHandle;

use crate::win;
use crate::MainWindow;

/// Toggle borderless fullscreen and reflect the new state in the UI.
pub(crate) fn toggle_fullscreen(ui: &MainWindow) {
    ui.set_fullscreen(win::toggle_fullscreen());
}

pub(crate) fn wire(ui: &MainWindow) {
    ui.on_toggle_fullscreen({
        let ui_w = ui.as_weak();
        move || { if let Some(ui) = ui_w.upgrade() { toggle_fullscreen(&ui); } }
    });
    ui.on_set_always_on_top({
        let ui_w = ui.as_weak();
        move |on| {
            win::set_always_on_top(on);
            if let Some(ui) = ui_w.upgrade() { ui.set_pinned(on); }
        }
    });
    ui.on_win_minimize(win::minimize);
    ui.on_win_maximize(win::toggle_maximize);
    ui.on_win_close(|| { let _ = slint::quit_event_loop(); });
    ui.on_start_window_drag(win::start_drag);
    // Closing via the OS (Alt+F4, taskbar menu, etc.) must quit the loop too.
    // With `run_event_loop_until_quit`, an unhandled close only *hides* the
    // window — the process and mpv's audio would keep running in the background,
    // and reopening would stack zombie instances that fight over input/audio.
    ui.window().on_close_requested(|| {
        let _ = slint::quit_event_loop();
        slint::CloseRequestResponse::HideWindow
    });
}
