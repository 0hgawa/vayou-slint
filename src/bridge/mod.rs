//! UI bridge: wires the Slint `MainWindow` callbacks to the services/mpv layer.
//! One module per domain — each owns its helpers and registers its own
//! callbacks in `wire`. `app` additionally drives the mpv lifecycle and the
//! event-to-UI marshalling.

use std::sync::Arc;

use crate::state::{AppState, MpvState};
use crate::MainWindow;

pub mod app;
pub mod keys;
pub mod panels;
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
