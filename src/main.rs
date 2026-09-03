// Vayou — libmpv video player in Slint.
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
// Claiming the media file types is a Windows registry affair the running program
// does for itself. Elsewhere the association comes from the `.desktop` entry a
// package installs, so there is nothing for the binary to register.
#[cfg(windows)]
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

/// The identity the desktop matches the running window against: the Wayland
/// `app_id` and the X11 `WM_CLASS`, both of which have to equal the basename of
/// the installed `vayou.desktop`.
///
/// winit sets neither unless asked — it only calls `set_app_id` when the
/// attributes carry a name — and with both unset the compositor cannot tie the
/// window to the entry: the dock and the switcher show a nameless window
/// wearing a fallback icon, however correctly the entry itself is installed.
#[cfg(unix)]
const APP_ID: &str = "vayou";

/// Select the backend with that identity attached, and spend the activation
/// token the launcher handed us on the window it was meant for.
///
/// The renderer choice still comes from `SLINT_BACKEND` (set below), which the
/// selector reads when the builder does not name one — this only adds the hook.
///
/// The token is the other half of `StartupNotify=true` in the `.desktop` entry:
/// the shell puts one in the environment, then runs a busy cursor and a pulsing
/// icon until a window comes back carrying it. winit sends it only when the
/// window attributes hold one, so leaving it unread does not merely cost the
/// window its focus — it leaves the launch feedback running until the shell
/// times it out, tens of seconds of spinner over a window that has been on
/// screen since millisecond fifty.
#[cfg(unix)]
fn announce_desktop_identity() {
    use slint::winit_030::winit::platform::startup_notify::{
        reset_activation_token_env, WindowAttributesExtStartupNotify,
    };
    use slint::winit_030::winit::platform::wayland::WindowAttributesExtWayland;
    use slint::winit_030::winit::platform::x11::WindowAttributesExtX11;
    use slint::winit_030::winit::window::ActivationToken;

    // Wayland's variable first: it is the one a session that set both is
    // actually speaking. Read here rather than through winit's
    // `read_token_from_env`, which wants an `ActiveEventLoop` that does not
    // exist this early.
    let token = std::env::var("XDG_ACTIVATION_TOKEN")
        .or_else(|_| std::env::var("DESKTOP_STARTUP_ID"))
        .ok()
        .map(ActivationToken::from_raw);

    // Unconditionally, and before any child is spawned: a token is single-use,
    // and one left in the environment is inherited by every ffmpeg we run,
    // which would spend ours raising a window of their own.
    reset_activation_token_env();

    // A `Cell`, because the hook runs at every window creation: handing an
    // already-spent token to a second window asks the compositor to raise it
    // over whatever the user had focused by then.
    let token = std::cell::Cell::new(token);

    // Both names, because a build runs on either display server and each reads
    // its own: Wayland the app_id, X11 the WM_CLASS.
    let selected = slint::BackendSelector::new()
        .with_winit_window_attributes_hook(move |attributes| {
            let attributes = WindowAttributesExtWayland::with_name(attributes, APP_ID, APP_ID);
            let attributes = WindowAttributesExtX11::with_name(attributes, APP_ID, APP_ID);
            match token.take() {
                Some(token) => attributes.with_activation_token(token),
                None => attributes,
            }
        })
        .select();
    if let Err(e) = selected {
        // Not fatal: the window still opens, it just will not be recognised as
        // this application by the shell.
        tracing::warn!(error = %e, "could not set the desktop identity");
    }
}

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
    #[cfg(windows)]
    std::thread::spawn(file_assoc::ensure_registered);

    // The OpenGL underlay needs the femtovg/GL renderer so the rendering
    // notifier yields a NativeOpenGL context for mpv to render into.
    if std::env::var_os("SLINT_BACKEND").is_none() {
        std::env::set_var("SLINT_BACKEND", "winit-femtovg");
    }

    // Must happen before the first window is built, since the hook runs at
    // window creation.
    #[cfg(unix)]
    announce_desktop_identity();

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

    // The font families this platform actually has, before the first frame is
    // drawn — the UI's own family and the list the subtitle picker offers.
    ui.set_ui_font(services::settings::UI_FONT.into());
    // Whether this install can rewrite itself. Asked once — the answer cannot
    // change while the binary runs — so the About page never probes the disk.
    ui.set_install_kind(update::install_kind().into());
    ui.set_sub_fonts(slint::ModelRc::new(slint::VecModel::from(
        services::settings::SUBTITLE_FONTS.iter().map(|f| slint::SharedString::from(*f)).collect::<Vec<_>>(),
    )));

    ui.show()?;

    // Files dropped onto the window — WM_DROPFILES in the Win32 subclass, an
    // ordinary winit event everywhere else.
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
    // Asked for once, when the render context is built — by which time winit has
    // created the native window this reads the display connection from.
    let native_display = {
        let ui_w = ui.as_weak();
        move || ui_w.upgrade().and_then(|ui| video_render::native_display(ui.window()))
    };
    if let Err(e) = video_render::install(ui.window(), mpv_state.clone(), request_redraw, on_ready, native_display) {
        tracing::error!(error = %e, "could not install the video render notifier");
    }
    // Kept for the shutdown below, which runs after both have been handed over.
    let (mpv_at_exit, app_at_exit) = (mpv_state.clone(), app_state.clone());
    bridge::app::spawn_mpv(ui.as_weak(), mpv_state, app_state);

    // The native window only exists once the event loop starts pumping, so keep
    // asking until it does, then hook it: file drops, plus whatever chrome the
    // platform hands over by hand. `attach_ui` reports whether the window was
    // there, which is the signal to stop asking.
    let init_timer = Rc::new(slint::Timer::default());
    {
        let (ui_w, t) = (ui.as_weak(), init_timer.clone());
        init_timer.start(slint::TimerMode::Repeated, Duration::from_millis(16), move || {
            let Some(ui) = ui_w.upgrade() else { return };
            if win::attach_ui(ui.window()) {
                t.stop();
            }
        });
    }

    slint::run_event_loop_until_quit()?;
    let _ = init_timer;

    // A slider released a fraction of a second before the window closed still
    // has its write sitting on a timer the event loop will never run again.
    bridge::persist::flush(&app_at_exit);

    // Quitting is the one way out that no mpv event announces: the playhead is
    // saved on END_FILE, on SHUTDOWN and every 30 seconds, and closing a playing
    // file fires none of them — so without this, reopening resumes up to half a
    // minute behind where the viewer actually stopped.
    if let Ok(mpv) = mpv_at_exit.get() {
        mpv::events::save_position(mpv, &app_at_exit);
    }

    // Leave without unwinding.
    //
    // Slint's clipboard support runs a worker thread that sits in
    // `wl_display_read_events` on the Wayland connection. Returning from `main`
    // tears that connection down underneath it, and the thread faults on it
    // often enough to matter — a segfault on close, intermittent, which costs
    // the user a core dump written synchronously before the process can die.
    // Measured here: ~250ms to close cleanly against ~1.4s when it crashes, on a
    // 478 KB test clip. The dump scales with whatever mpv was holding, so a real
    // film makes that wait several seconds, which is what "closing hangs" was.
    //
    // There is nothing left worth releasing by hand: the settings are on disk
    // (above), and the kernel reclaims the rest better than the teardown does.
    std::process::exit(0);
}
