//! App lifecycle: spawn mpv on a background thread, marshal its events onto the
//! UI thread, and load files (CLI / drop / dialog / URL).

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use slint::ComponentHandle;

use crate::bridge::playback::{load_chapters, push_ab};
use crate::bridge::subtitle::{apply_sub_style, start_translation};
use crate::error::LogErr;
use crate::mpv::events::{start_event_loop, EventSink, PlayerEvent};
use crate::mpv::player::MpvPlayer;
use crate::mpv::types::{MPV_FORMAT_DOUBLE, MPV_FORMAT_FLAG, MPV_FORMAT_STRING};
use crate::services::playback::PlaybackService;
use crate::services::playlist::PlaylistService;
use crate::services::tracks::TracksService;
use crate::state::{set_pending_resume, AppState, MpvState};
use crate::MainWindow;

/// Create mpv on a background thread (its `warm_up` blocks ~1-2s on cold start,
/// which must NOT freeze the UI) and start the event loop. Video output is the
/// OpenGL underlay (`video_render`), whose render context is created lazily in
/// the rendering notifier. Once mpv is up, requests a redraw (so the notifier
/// fires and builds the render context); the CLI file is then opened from
/// `video_render::install`'s `on_ready`.
pub(crate) fn spawn_mpv(weak: slint::Weak<MainWindow>, mpv_state: Arc<MpvState>, app_state: Arc<AppState>) {
    std::thread::spawn(move || {
        let mpv = match MpvPlayer::new() {
            Ok(m) => m,
            Err(e) => {
                tracing::error!(error = %e, "mpv init failed");
                return;
            }
        };

        let _ = mpv.observe_property("time-pos", 1, MPV_FORMAT_DOUBLE);
        let _ = mpv.observe_property("duration", 2, MPV_FORMAT_DOUBLE);
        let _ = mpv.observe_property("pause", 3, MPV_FORMAT_FLAG);
        let _ = mpv.observe_property("volume", 4, MPV_FORMAT_DOUBLE);
        let _ = mpv.observe_property("media-title", 5, MPV_FORMAT_STRING);

        if let Ok((alang, slang, vol_boost, embedded_styles, sub_codepage, volume, speed)) = app_state.with(|s, _| {
            (s.preferred_audio_lang.clone(), s.preferred_subtitle_lang.clone(), s.volume_boost, s.apply_embedded_styles, s.subtitle_encoding.clone(), s.volume, s.speed)
        }) {
            if !alang.is_empty() { mpv.set::<&str>("alang", &alang).log_err("set alang"); }
            if !slang.is_empty() { mpv.set::<&str>("slang", &slang).log_err("set slang"); }
            mpv.set::<&str>("volume-max", if vol_boost { "200" } else { "100" }).log_err("set volume-max");
            mpv.set::<&str>("sub-ass-override", if embedded_styles { "no" } else { "force" }).log_err("set sub-ass-override");
            if !sub_codepage.is_empty() { mpv.set::<&str>("sub-codepage", &sub_codepage).log_err("set sub-codepage"); }
            // Apply the persisted default volume/speed to mpv so playback starts
            // at the saved level (the UI was already seeded with these values).
            PlaybackService::set_volume(&mpv, volume).log_err("set initial volume");
            PlaybackService::set_speed(&mpv, speed).log_err("set initial speed");
        }

        if mpv_state.init(mpv).is_err() {
            return;
        }
        let Ok(mpv_arc) = mpv_state.get().map(Arc::clone) else { return };
        let sink = make_sink(weak.clone(), mpv_state.clone(), app_state.clone());
        start_event_loop(mpv_arc, app_state.clone(), sink);

        // Kick one redraw so the rendering notifier fires now that mpv exists
        // and builds the render context (which is what makes mpv's vo usable —
        // see `video_render::install`'s `on_ready`).
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(ui) = weak.upgrade() {
                ui.window().request_redraw();
            }
        });
    });
}

/// Build the event sink that marshals mpv events onto the Slint UI thread.
fn make_sink(weak: slint::Weak<MainWindow>, mpv_state: Arc<MpvState>, app_state: Arc<AppState>) -> EventSink {
    Arc::new(move |ev: PlayerEvent| {
        let (weak, mpv_state, app_state) = (weak.clone(), mpv_state.clone(), app_state.clone());
        let _ = slint::invoke_from_event_loop(move || {
            let Some(ui) = weak.upgrade() else { return };
            match ev {
                PlayerEvent::TimePos(t) => ui.set_current_time(t as f32),
                PlayerEvent::Duration(d) => ui.set_duration(d as f32),
                PlayerEvent::Pause(p) => ui.set_playing(ui.get_has_file() && !p),
                PlayerEvent::Volume(v) => ui.set_volume(v as f32),
                PlayerEvent::MediaTitle(t) => ui.set_media_title(t.into()),
                PlayerEvent::FileLoaded => {
                    ui.set_has_file(true);
                    ui.set_tr_active(false);
                    ui.set_tr_error(String::new().into());
                    let auto_play = app_state.with(|s, _| s.auto_play).unwrap_or(true);
                    if let Ok(mpv) = mpv_state.get() {
                        load_chapters(&ui, mpv);
                        apply_sub_style(mpv, &app_state);
                        // Honour the "Auto Play" preference. Set it explicitly
                        // either way: mpv keeps the previous file's `pause` state
                        // across `loadfile`, so without an explicit play a new
                        // file opened while paused would stay paused.
                        if auto_play {
                            PlaybackService::play(mpv).log_err("auto-play");
                        } else {
                            PlaybackService::pause(mpv).log_err("pause on load");
                        }
                    }
                    // Sync the play/pause icon now: the mpv "pause" event can arrive
                    // before has-file is set, which would otherwise leave the icon
                    // stuck on "play" until the next manual pause/resume.
                    ui.set_playing(auto_play);
                    push_ab(&ui);
                    // Auto-translate the new file if a target language is set and a
                    // sub track is selected (deferred so mpv has registered tracks).
                    if app_state.with(|s, _| s.translate_lang.clone()).is_ok_and(|l| l != "off") {
                        let (w, mpv2, app2) = (ui.as_weak(), mpv_state.clone(), app_state.clone());
                        slint::Timer::single_shot(Duration::from_millis(800), move || {
                            let (Some(ui), Ok(mpv)) = (w.upgrade(), mpv2.get()) else { return };
                            if TracksService::get_all(mpv).iter().any(|t| t.track_type == "sub" && t.selected) {
                                start_translation(&ui, &mpv2, &app2);
                            }
                        });
                    }
                }
                PlayerEvent::EndFile => ui.set_playing(false),
            }
        });
    })
}

/// Open a file or URL: save the current file's position, queue a resume for the
/// new one, then load it (with sibling playlist for local files).
pub(crate) fn open_file(path: &str, mpv_state: &Arc<MpvState>, app_state: &Arc<AppState>) {
    let is_url = path.starts_with("http://") || path.starts_with("https://");
    if !is_url && !Path::new(path).exists() {
        return;
    }
    let Ok(mpv) = mpv_state.get() else { return };

    let _ = app_state.with(|settings, current_file| {
        if let Some(prev) = current_file.clone() {
            let pos = mpv.get::<f64>("time-pos").unwrap_or(0.0);
            let title = mpv.get_property_string("media-title").unwrap_or_default();
            settings.touch_recent(&prev, &title, pos);
        }
    });

    let resume = app_state.with(|s, _| if s.remember_position { s.get_saved_position(path) } else { None }).ok().flatten();
    if let Some(pos) = resume {
        set_pending_resume(pos);
    }

    if is_url {
        mpv.command(&["loadfile", path, "replace"]).log_err("load URL");
    } else {
        PlaylistService::open_with_siblings(mpv, path).log_err("open file");
    }

    let title = Path::new(path).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let _ = app_state.with(|settings, current_file| {
        *current_file = Some(path.to_string());
        settings.touch_recent(path, &title, 0.0);
        settings.save().log_err("save recent file");
    });
}

/// The native open-file dialog (video + audio filters).
fn pick_file() -> Option<String> {
    rfd::FileDialog::new()
        .add_filter("Video", &["mp4", "mkv", "avi", "mov", "wmv", "flv", "webm", "mpg", "mpeg", "m4v", "3gp", "ts", "vob"])
        .add_filter("Audio", &["mp3", "flac", "wav", "ogg", "m4a", "aac", "opus", "wma"])
        .add_filter("All", &["*"])
        .pick_file()
        .map(|p| p.to_string_lossy().into_owned())
}

pub(crate) fn open_via_dialog(ui: &MainWindow, mpv_state: &Arc<MpvState>, app_state: &Arc<AppState>) {
    if let Some(p) = pick_file() {
        ui.set_has_file(true);
        open_file(&p, mpv_state, app_state);
    }
}

pub(crate) fn wire(ui: &MainWindow, mpv_state: &Arc<MpvState>, app_state: &Arc<AppState>) {
    ui.on_open_file({
        let (ui_w, mpv, app) = (ui.as_weak(), mpv_state.clone(), app_state.clone());
        move || { if let Some(ui) = ui_w.upgrade() { open_via_dialog(&ui, &mpv, &app); } }
    });
    ui.on_open_url({
        let ui_w = ui.as_weak();
        move || { if let Some(ui) = ui_w.upgrade() { ui.set_url_show(true); } }
    });
    ui.on_submit_url({
        let (ui_w, mpv, app) = (ui.as_weak(), mpv_state.clone(), app_state.clone());
        move |url| {
            let u = url.trim().to_string();
            if let Some(ui) = ui_w.upgrade() { ui.set_url_show(false); }
            if !u.is_empty() {
                if let Some(ui) = ui_w.upgrade() { ui.set_has_file(true); }
                open_file(&u, &mpv, &app);
            }
        }
    });
}
