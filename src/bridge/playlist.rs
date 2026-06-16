//! Playlist panel callbacks + the playlist-row refresh helper.

use std::sync::Arc;

use slint::{ComponentHandle, ModelRc, VecModel};

use crate::mpv::player::MpvPlayer;
use crate::services::playlist::PlaylistService;
use crate::state::MpvState;
use crate::{MainWindow, PlaylistRow};

/// Rebuild the playlist model from mpv.
pub(crate) fn refresh_playlist(ui: &MainWindow, mpv: &MpvPlayer) {
    let rows: Vec<PlaylistRow> = PlaylistService::get_all(mpv)
        .into_iter()
        .map(|p| PlaylistRow { index: p.index as i32, title: p.title.into(), current: p.current })
        .collect();
    ui.set_playlist_items(ModelRc::new(VecModel::from(rows)));
}

pub(crate) fn wire(ui: &MainWindow, mpv_state: &Arc<MpvState>) {
    ui.on_playlist_play({
        let (ui_w, mpv) = (ui.as_weak(), mpv_state.clone());
        move |idx| { if let Ok(m) = mpv.get() { let _ = PlaylistService::play_index(m, i64::from(idx)); if let Some(ui) = ui_w.upgrade() { refresh_playlist(&ui, m); } } }
    });
    ui.on_playlist_remove({
        let (ui_w, mpv) = (ui.as_weak(), mpv_state.clone());
        move |idx| { if let Ok(m) = mpv.get() { let _ = PlaylistService::remove(m, i64::from(idx)); if let Some(ui) = ui_w.upgrade() { refresh_playlist(&ui, m); } } }
    });
    ui.on_playlist_clear({
        let (ui_w, mpv) = (ui.as_weak(), mpv_state.clone());
        move || { if let Ok(m) = mpv.get() { let _ = PlaylistService::clear(m); if let Some(ui) = ui_w.upgrade() { refresh_playlist(&ui, m); } } }
    });
    ui.on_playlist_add_files({
        let (ui_w, mpv) = (ui.as_weak(), mpv_state.clone());
        move || {
            let Some(files) = rfd::FileDialog::new().add_filter("Media", crate::services::playlist::MEDIA_EXTENSIONS).add_filter("All", &["*"]).pick_files() else { return };
            if let Ok(m) = mpv.get() {
                for f in &files { let _ = PlaylistService::add(m, &f.to_string_lossy()); }
                if let Some(ui) = ui_w.upgrade() { refresh_playlist(&ui, m); }
            }
        }
    });
    ui.on_playlist_add_folder({
        let (ui_w, mpv) = (ui.as_weak(), mpv_state.clone());
        move || {
            let Some(dir) = rfd::FileDialog::new().pick_folder() else { return };
            if let Ok(m) = mpv.get() { let _ = PlaylistService::add(m, &dir.to_string_lossy()); if let Some(ui) = ui_w.upgrade() { refresh_playlist(&ui, m); } }
        }
    });
    ui.on_cycle_repeat({
        let (ui_w, mpv) = (ui.as_weak(), mpv_state.clone());
        move || {
            let Some(ui) = ui_w.upgrade() else { return };
            let next = match ui.get_repeat_mode().as_str() { "off" => "all", "all" => "one", _ => "off" };
            ui.set_repeat_mode(next.into());
            if let Ok(m) = mpv.get() {
                let _ = m.set::<&str>("loop-playlist", if next == "all" { "inf" } else { "no" });
                let _ = m.set::<&str>("loop-file", if next == "one" { "inf" } else { "no" });
            }
        }
    });
    ui.on_toggle_shuffle({
        let (ui_w, mpv) = (ui.as_weak(), mpv_state.clone());
        move || {
            let Some(ui) = ui_w.upgrade() else { return };
            let on = !ui.get_shuffle();
            ui.set_shuffle(on);
            if let Ok(m) = mpv.get() {
                let _ = m.command(&[if on { "playlist-shuffle" } else { "playlist-unshuffle" }]);
                refresh_playlist(&ui, m);
            }
        }
    });
}
