//! UI bridge: wires the Slint `MainWindow` callbacks to the services/mpv layer.
//! One module per domain — each owns its helpers and registers its own
//! callbacks in `wire`. `app` additionally drives the mpv lifecycle and the
//! event-to-UI marshalling.

use std::sync::{Arc, OnceLock};

use crate::state::{AppState, MpvState};
use crate::MainWindow;

/// Shared tokio runtime for off-thread work (HTTP search/translate, ffmpeg, and
/// the native file dialog). Lives for the whole program; results marshal back
/// via `slint::invoke_from_event_loop`.
pub(crate) fn runtime() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("tokio runtime"))
}

/// Run a native file dialog OFF the UI thread, then hand the picked value back
/// to the UI thread. Every rfd picker must go through here: a blocking dialog on
/// the event-loop thread stalls the render loop — the video image freezes (audio
/// keeps going) and folder browsing fights mpv's decode for the CPU.
pub(crate) fn pick_async<T: Send + 'static>(
    pick: impl FnOnce() -> Option<T> + Send + 'static,
    then: impl FnOnce(T) + Send + 'static,
) {
    runtime().spawn_blocking(move || {
        if let Some(v) = pick() {
            let _ = slint::invoke_from_event_loop(move || then(v));
        }
    });
}

pub mod app;
pub mod keys;
pub mod panels;
pub mod persist;
pub mod playback;
pub mod playlist;
pub mod settings;
pub mod subtitle;
pub mod tracks;
pub mod video;
pub mod window;

/// Register every domain's callbacks on the window.
pub fn wire(ui: &MainWindow, mpv_state: &Arc<MpvState>, app_state: &Arc<AppState>) {
    app::wire(ui, mpv_state, app_state);
    playback::wire(ui, mpv_state, app_state);
    window::wire(ui);
    tracks::wire(ui, mpv_state, app_state);
    subtitle::wire(ui, mpv_state, app_state);
    video::wire(ui, mpv_state, app_state);
    playlist::wire(ui, mpv_state);
    settings::wire(ui, mpv_state, app_state);
    panels::wire(ui, mpv_state, app_state);
    keys::wire(ui, mpv_state, app_state);
}
