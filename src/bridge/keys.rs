//! The keyboard handler: fixed numpad pan/zoom + F11, then the rebindable
//! shortcut table dispatched to the relevant domain.

use std::sync::Arc;

use slint::ComponentHandle;

use crate::bridge::app::open_via_dialog;
use crate::bridge::panels::refresh_panel;
use crate::bridge::playback::{adjust_volume, push_ab, screenshot_to_toast, set_speed, toggle_mute};
use crate::bridge::tracks::cycle_track;
use crate::bridge::video::{cycle_aspect, nudge_pan, nudge_zoom};
use crate::bridge::window::toggle_fullscreen;
use crate::error::LogErr;
use crate::keybindings;
use crate::services::playback::PlaybackService;
use crate::services::playlist::PlaylistService;
use crate::services::video::VideoService;
use crate::state::{AppState, MpvState};
use crate::MainWindow;

/// Handle a key event: non-rebindable numpad pan/zoom + F11 first, then resolve
/// against the (rebindable) keybindings and run the matching action.
fn handle_key(ui: &MainWindow, mpv_state: &Arc<MpvState>, app_state: &Arc<AppState>, key: &str, ctrl: bool, shift: bool, alt: bool) {
    let Ok(mpv) = mpv_state.get().map(Arc::clone) else { return };
    // Numpad pan/zoom + F11 are fixed (not rebindable), matching the original.
    match key {
        "8" => return nudge_pan(mpv_state, 0.0, -0.02),
        "2" => return nudge_pan(mpv_state, 0.0, 0.02),
        "4" => return nudge_pan(mpv_state, 0.02, 0.0),
        "6" => return nudge_pan(mpv_state, -0.02, 0.0),
        "5" => return VideoService::reset_zoom_pan(&mpv).log_err("reset zoom/pan"),
        "*" => return nudge_zoom(mpv_state, 0.1),
        "/" => return nudge_zoom(mpv_state, -0.1),
        "F11" => return toggle_fullscreen(ui),
        _ => {}
    }

    let custom = app_state.with(|s, _| s.keybindings.clone()).unwrap_or_default();
    let Some(action) = keybindings::resolve(&custom, key, ctrl, shift, alt) else { return };
    match action {
        "togglePause" => PlaybackService::toggle_pause(&mpv).log_err("toggle pause"),
        "seekForward" => PlaybackService::seek_relative(&mpv, 5.0).log_err("seek"),
        "seekForwardLong" => PlaybackService::seek_relative(&mpv, 30.0).log_err("seek"),
        "seekBack" => PlaybackService::seek_relative(&mpv, -5.0).log_err("seek"),
        "seekBackLong" => PlaybackService::seek_relative(&mpv, -30.0).log_err("seek"),
        "nextFile" => PlaylistService::next(&mpv).log_err("next file"),
        "prevFile" => PlaylistService::prev(&mpv).log_err("previous file"),
        "frameNext" => PlaybackService::frame_step(&mpv).log_err("frame step"),
        "framePrev" => PlaybackService::frame_back_step(&mpv).log_err("frame back"),
        "speedUp" => set_speed(ui, mpv_state, (ui.get_speed() + 0.25).min(4.0)),
        "speedDown" => set_speed(ui, mpv_state, (ui.get_speed() - 0.25).max(0.25)),
        "abLoop" => { PlaybackService::cycle_ab_loop(&mpv); push_ab(ui); }
        "volumeUp" => adjust_volume(ui, mpv_state, app_state, 5.0),
        "volumeDown" => adjust_volume(ui, mpv_state, app_state, -5.0),
        "mute" => toggle_mute(ui, mpv_state),
        "fullscreen" => toggle_fullscreen(ui),
        "screenshot" => screenshot_to_toast(ui, mpv_state),
        "aspectRatio" => cycle_aspect(mpv_state),
        "cycleSub" => cycle_track(mpv_state, "sub"),
        "cycleAudio" => cycle_track(mpv_state, "audio"),
        "openFile" => open_via_dialog(ui, mpv_state, app_state),
        "openUrl" => ui.invoke_open_url(),
        "mediaInfo" => { ui.set_active_panel("info".into()); refresh_panel(ui, mpv_state, app_state, "info"); }
        _ => {}
    }
}

pub(crate) fn wire(ui: &MainWindow, mpv_state: &Arc<MpvState>, app_state: &Arc<AppState>) {
    ui.on_key({
        let (ui_w, mpv, app) = (ui.as_weak(), mpv_state.clone(), app_state.clone());
        move |key, ctrl, shift, alt| {
            if let Some(ui) = ui_w.upgrade() {
                handle_key(&ui, &mpv, &app, key.as_str(), ctrl, shift, alt);
            }
        }
    });
}
