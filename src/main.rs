// Vayou Native — libmpv video player in Slint.
//
// A single frameless Slint window. mpv renders the video (subtitles included)
// into the window's OpenGL framebuffer UNDER the UI via Slint's rendering
// notifier (the recommended "OpenGL underlay" approach — see `video_render`).
// Single process, GPU video, no WebView, no second window.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![deny(unsafe_op_in_unsafe_fn)]

use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use slint::ComponentHandle;

mod bridge;
mod error;
mod file_assoc;
mod keybindings;
mod mpv;
mod services;
mod state;
mod translate_job;
mod update;
mod util;
mod video_render;
mod win;

use state::{AppState, MpvState};

slint::include_modules!();

/// Logging to stderr: warn+ by default; `VAYOU_LOG=debug` for dev detail.
fn install_tracing() {
    let level = match std::env::var("VAYOU_LOG").as_deref() {
        Ok("trace") => tracing::Level::TRACE,
        Ok("debug") => tracing::Level::DEBUG,
        Ok("info") => tracing::Level::INFO,
        Ok("error") => tracing::Level::ERROR,
        _ => tracing::Level::WARN,
    };
    let _ = tracing_subscriber::fmt().with_max_level(level).with_target(false).compact().try_init();
}

fn main() -> Result<(), slint::PlatformError> {
    install_tracing();

    // Register file associations off-thread so it never delays startup.
    std::thread::spawn(file_assoc::ensure_registered);

    // The OpenGL underlay needs the femtovg/GL renderer so the rendering
    // notifier yields a NativeOpenGL context for mpv to render into.
    if std::env::var_os("SLINT_BACKEND").is_none() {
        std::env::set_var("SLINT_BACKEND", "winit-femtovg");
    }

    let mpv_state = Arc::new(MpvState::default());
    let app_state = Arc::new(AppState::default());

    // Apply the saved UI language before any window is built.
    if let Ok(lang) = app_state.with(|s, _| s.language.clone()) {
        if !lang.is_empty() && lang != "en" {
            let _ = slint::select_bundled_translation(&lang);
        }
    }

    let ui = MainWindow::new()?;

    if let Ok((vol, spd, tl)) = app_state.with(|s, _| (s.volume, s.speed, s.translate_lang.clone())) {
        ui.set_volume(vol as f32);
        ui.set_speed(spd as f32);
        // Seed the translate language so auto-translate (gated on this property
        // in the FileLoaded handler) fires for a persisted target on first load.
        ui.set_translate_lang(tl.into());
    }
    ui.set_max_volume(bridge::playback::max_volume(&app_state));

    ui.show()?;

    // Files dropped onto the window (via WM_DROPFILES in the subclass).
    win::set_drop_handler({
        let (ui_w, mpv, app) = (ui.as_weak(), mpv_state.clone(), app_state.clone());
        move |path| {
            if let Some(ui) = ui_w.upgrade() { ui.set_has_file(true); }
            bridge::app::open_file(&path, &mpv, &app);
        }
    });

    bridge::wire(&ui, &mpv_state, &app_state);

    // The file passed on the command line (Explorer "Open with" / double-click).
    let cli = std::env::args().skip(1).find(|a| !a.starts_with('-') && std::path::Path::new(a).exists());

    // Install the OpenGL underlay: mpv draws each frame into this window's
    // framebuffer, under the Slint UI, via the rendering notifier. mpv itself is
    // created on a background thread (warm-up blocks); the render context is
    // built lazily once it's up. The CLI file is opened from `on_ready` — only
    // after the render context exists — so mpv's vo is ready and keeps the video
    // track (loading earlier makes mpv drop video for "no render context").
    let request_redraw = {
        let ui_w = ui.as_weak();
        move || {
            let ui_w = ui_w.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(ui) = ui_w.upgrade() {
                    ui.window().request_redraw();
                }
            });
        }
    };
    let on_ready = {
        let (ui_w, mpv2, app2) = (ui.as_weak(), mpv_state.clone(), app_state.clone());
        move || {
            let Some(path) = cli else { return };
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(ui) = ui_w.upgrade() {
                    ui.set_has_file(true);
                }
                bridge::app::open_file(&path, &mpv2, &app2);
            });
        }
    };
    if let Err(e) = video_render::install(ui.window(), mpv_state.clone(), request_redraw, on_ready) {
        tracing::error!(error = %e, "could not install the video render notifier");
    }
    bridge::app::spawn_mpv(ui.as_weak(), mpv_state, app_state);

    // Once the Slint window's HWND exists (only after the event loop starts
    // pumping), hook it: drag-drop, app icon, frameless chrome, rounded corners.
    let init_timer = Rc::new(slint::Timer::default());
    {
        let (ui_w, t) = (ui.as_weak(), init_timer.clone());
        init_timer.start(slint::TimerMode::Repeated, Duration::from_millis(16), move || {
            let Some(ui) = ui_w.upgrade() else { return };
            if win::hwnd_of(ui.window()).is_none() {
                return;
            }
            win::attach_ui(ui.window());
            t.stop();
        });
    }

    slint::run_event_loop_until_quit()?;
    let _ = init_timer;
    Ok(())
}
