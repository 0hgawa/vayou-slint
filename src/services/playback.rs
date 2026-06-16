use tracing::info;

use crate::error::MpvError;
use crate::mpv::player::MpvPlayer;
use crate::state;

/// Pure playback logic — no UI dependency.
pub struct PlaybackService;

impl PlaybackService {
    pub fn toggle_pause(mpv: &MpvPlayer) -> Result<(), MpvError> {
        mpv.command(&["cycle", "pause"])
    }

    pub fn pause(mpv: &MpvPlayer) -> Result<(), MpvError> {
        mpv.set("pause", true)
    }

    pub fn play(mpv: &MpvPlayer) -> Result<(), MpvError> {
        mpv.set("pause", false)
    }

    pub fn seek_relative(mpv: &MpvPlayer, seconds: f64) -> Result<(), MpvError> {
        mpv.command(&["seek", &seconds.to_string(), "relative"])
    }

    pub fn seek_absolute(mpv: &MpvPlayer, seconds: f64) -> Result<(), MpvError> {
        mpv.command(&["seek", &seconds.to_string(), "absolute"])
    }

    pub fn set_volume(mpv: &MpvPlayer, volume: f64) -> Result<(), MpvError> {
        mpv.set("volume", volume)
    }

    pub fn set_speed(mpv: &MpvPlayer, speed: f64) -> Result<(), MpvError> {
        mpv.set("speed", speed)
    }

    pub fn screenshot(mpv: &MpvPlayer, path: &str) -> Result<(), MpvError> {
        mpv.command(&["screenshot-to-file", path, "subtitles"])
    }

    pub fn frame_step(mpv: &MpvPlayer) -> Result<(), MpvError> {
        mpv.command(&["frame-step"])
    }

    pub fn frame_back_step(mpv: &MpvPlayer) -> Result<(), MpvError> {
        mpv.command(&["frame-back-step"])
    }

    /// Cycle A → B → clear. Snapshots time-pos from mpv directly (no UI
    /// latency). Loop enforcement happens in the event loop.
    pub fn cycle_ab_loop(mpv: &MpvPlayer) -> AbLoopState {
        let pos = mpv.get::<f64>("time-pos").unwrap_or(0.0);
        let (a, b) = (state::ab_loop::get_a(), state::ab_loop::get_b());

        let new_state = match (a, b) {
            (None, _) => {
                state::ab_loop::set_a(Some(pos));
                AbLoopState { a: Some(pos), b: None }
            }
            (Some(a_val), None) if pos > a_val => {
                state::ab_loop::set_b(Some(pos));
                AbLoopState { a, b: Some(pos) }
            }
            (Some(_), None) => {
                // pos <= A: replace A rather than creating an invalid B<A range.
                state::ab_loop::set_a(Some(pos));
                AbLoopState { a: Some(pos), b: None }
            }
            (Some(_), Some(_)) => {
                state::ab_loop::clear();
                AbLoopState { a: None, b: None }
            }
        };

        info!(a = ?new_state.a, b = ?new_state.b, "ab-loop cycled");
        new_state
    }

    pub fn set_ab_loop_a(time: Option<f64>) {
        state::ab_loop::set_a(time);
    }

    pub fn set_ab_loop_b(time: Option<f64>) {
        state::ab_loop::set_b(time);
    }

    pub fn clear_ab_loop() {
        state::ab_loop::clear();
    }

    pub fn get_chapters(mpv: &MpvPlayer) -> Vec<Chapter> {
        let count: i64 = mpv.get_num("chapter-list/count", 0);
        let current: i64 = mpv.get_num("chapter", -1);

        (0..count)
            .filter_map(|i| {
                let title = mpv.get_property_string(&format!("chapter-list/{i}/title")).unwrap_or_else(|_| format!("Chapter {}", i + 1));
                let time: f64 = mpv.get_property_string(&format!("chapter-list/{i}/time")).ok()?.parse().ok()?;
                Some(Chapter { title, time, current: i == current })
            })
            .collect()
    }

    pub fn seek_chapter(mpv: &MpvPlayer, index: i64) -> Result<(), MpvError> {
        mpv.set::<&str>("chapter", &index.to_string())
    }
}

#[derive(Clone, Copy)]
pub struct AbLoopState {
    pub a: Option<f64>,
    pub b: Option<f64>,
}

pub struct Chapter {
    pub title: String,
    pub time: f64,
    pub current: bool,
}
