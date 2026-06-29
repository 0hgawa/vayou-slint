use std::ffi::CStr;
use std::os::raw::c_char;
use std::sync::Arc;
use std::time::Instant;

use tracing::{debug, info};

use super::player::MpvPlayer;
use super::types::{MPV_EVENT_PROPERTY_CHANGE, MPV_EVENT_FILE_LOADED, MPV_EVENT_END_FILE, MPV_EVENT_SHUTDOWN, MpvEvent, MpvEventProperty, MPV_FORMAT_DOUBLE, MPV_FORMAT_FLAG, MPV_FORMAT_STRING};
use crate::error::LogErr;
use crate::state::{self, take_pending_resume, AppState};

/// A typed player event, forwarded to the UI thread by the sink. Mirrors the
/// `mpv:*` Tauri events the WebView build emitted.
pub enum PlayerEvent {
    TimePos(f64),
    Duration(f64),
    Pause(bool),
    Volume(f64),
    MediaTitle(String),
    FileLoaded,
    EndFile,
}

/// Receives player events on the mpv-events thread. The implementation
/// (in `main`) marshals them onto the Slint UI thread.
pub type EventSink = Arc<dyn Fn(PlayerEvent) + Send + Sync>;

/// Spawn a named background thread that polls mpv events and forwards them to
/// the sink as typed `PlayerEvent`s.
pub fn start_event_loop(mpv: Arc<MpvPlayer>, app: Arc<AppState>, sink: EventSink) {
    std::thread::Builder::new()
        .name("mpv-events".into())
        .spawn(move || {
            info!("Event loop started");
            run_loop(&mpv, &app, &sink);
            info!("Event loop ended");
        })
        .expect("Failed to spawn mpv event loop thread");
}

fn run_loop(mpv: &MpvPlayer, app: &AppState, sink: &EventSink) {
    let mut last_save = Instant::now();
    // Throttle the high-frequency time-pos updates (~60Hz from mpv) to ~15Hz so
    // the UI thread isn't asked to repaint the whole window every frame — that
    // was the main source of sluggishness. Other events pass through instantly.
    let mut last_timepos = Instant::now();

    loop {
        let evt = mpv.wait_event(0.05);

        match evt.event_id {
            MPV_EVENT_PROPERTY_CHANGE => {
                if evt.data.is_null() {
                    continue;
                }
                if let Some(pe) = read_property_change(evt) {
                    if matches!(pe, PlayerEvent::TimePos(_)) {
                        if last_timepos.elapsed() >= std::time::Duration::from_millis(66) {
                            last_timepos = Instant::now();
                            sink(pe);
                        }
                    } else {
                        sink(pe);
                    }
                }
            }

            MPV_EVENT_FILE_LOADED => {
                debug!("File loaded");
                state::ab_loop::clear();
                if let Some(pos) = take_pending_resume() {
                    mpv.command(&["seek", &pos.to_string(), "absolute"]).log_err("resume seek");
                }
                restore_saved_tracks(mpv, app);
                sink(PlayerEvent::FileLoaded);
            }

            MPV_EVENT_END_FILE => {
                debug!("End of file");
                save_position(mpv, app);
                sink(PlayerEvent::EndFile);
            }

            MPV_EVENT_SHUTDOWN => {
                info!("mpv shutdown event");
                save_position(mpv, app);
                break;
            }

            _ => {}
        }

        // AB loop enforcement runs on every loop iteration (~20Hz), not on
        // time-pos events — those can be coalesced/throttled by mpv to as low
        // as 1Hz on some containers, which would let the playhead overshoot B
        // by seconds before triggering.
        enforce_ab_loop(mpv);

        // Save position every 30 seconds.
        if last_save.elapsed().as_secs() >= 30 {
            save_position(mpv, app);
            last_save = Instant::now();
        }
    }
}

/// Manual A-B loop enforcement. Cheap when not armed (2 atomic loads). When
/// armed, polls time-pos directly from mpv and seeks if past B.
fn enforce_ab_loop(mpv: &MpvPlayer) {
    if !state::ab_loop::is_armed() { return; }
    let Ok(pos) = mpv.get::<f64>("time-pos") else { return };
    if let Some(target) = state::ab_loop::check(pos) {
        mpv.command(&["seek", &target.to_string(), "absolute+exact"]).log_err("ab-loop seek");
        debug!(target, "ab-loop: seek back to A");
    }
}

fn restore_saved_tracks(mpv: &MpvPlayer, app: &AppState) {
    let _ = app.with(|settings, current_file| {
        if !settings.remember_selections { return; }
        let Some(path) = current_file.as_ref() else { return };
        let (audio, sub) = settings.get_saved_tracks(path);
        if let Some(id) = audio {
            mpv.set::<&str>("aid", &id.to_string()).log_err("restore audio track");
        }
        if let Some(id) = sub {
            mpv.set::<&str>("sid", &id.to_string()).log_err("restore subtitle track");
        }
    });
}

fn save_position(mpv: &MpvPlayer, app: &AppState) {
    let pos = mpv.get::<f64>("time-pos").unwrap_or(0.0);
    if pos <= 1.0 { return; }
    let _ = app.with(|settings, current_file| {
        if let Some(path) = current_file.as_ref() {
            let title = mpv.get_property_string("filename").unwrap_or_default();
            settings.touch_recent(path, &title, pos);
            settings.save().log_err("save position");
        }
    });
}

fn read_property_change(evt: &MpvEvent) -> Option<PlayerEvent> {
    let prop = unsafe { &*(evt.data as *const MpvEventProperty) };
    if prop.name.is_null() || prop.data.is_null() {
        return None; // Property value unavailable (e.g. during init).
    }
    let name = unsafe { CStr::from_ptr(prop.name).to_str().ok()? };
    unsafe {
        match (name, prop.format) {
            ("time-pos", MPV_FORMAT_DOUBLE) => Some(PlayerEvent::TimePos(*(prop.data as *const f64))),
            ("duration", MPV_FORMAT_DOUBLE) => Some(PlayerEvent::Duration(*(prop.data as *const f64))),
            ("pause", MPV_FORMAT_FLAG) => Some(PlayerEvent::Pause(*(prop.data as *const i32) != 0)),
            ("volume", MPV_FORMAT_DOUBLE) => Some(PlayerEvent::Volume(*(prop.data as *const f64))),
            ("filename", MPV_FORMAT_STRING) => {
                let ptr = *(prop.data as *const *const c_char);
                if ptr.is_null() {
                    return None;
                }
                CStr::from_ptr(ptr).to_str().ok().map(|s| PlayerEvent::MediaTitle(s.to_owned()))
            }
            _ => None,
        }
    }
}
