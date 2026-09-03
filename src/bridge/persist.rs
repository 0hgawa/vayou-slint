//! Deferred settings writes.
//!
//! Six controls emit continuously while dragged — subtitle size, border,
//! shadow and position, plus the default volume and speed — and each emission
//! used to rewrite the whole config file. One drag across a slider is dozens of
//! full serialise-and-write cycles for a value the user has not finished
//! choosing, and the last one is the only one worth keeping.
//!
//! What is deferred is only the disk write. Everything the user can see or hear
//! — mpv's subtitle style, the slider itself — stays immediate, so this is
//! invisible except in the I/O it does not do.

use std::cell::Cell;
use std::sync::Arc;
use std::time::Duration;

use crate::error::LogErr;
use crate::state::AppState;

/// How long the settings have to stay unchanged before they are written. Long
/// enough that a drag produces one write rather than dozens, short enough that
/// a crash in that window costs nothing anyone would notice.
const QUIET: Duration = Duration::from_millis(400);

thread_local! {
    /// Restarted on every change, so it only ever fires after the last one.
    static TIMER: slint::Timer = slint::Timer::default();
    /// Whether a change is waiting on the timer. Read by `flush` on the way
    /// out, so a quit mid-drag still keeps the value.
    static PENDING: Cell<bool> = const { Cell::new(false) };
}

/// Persist the settings once the user stops moving the control.
///
/// UI thread only: the timer and its flag live there, which is also where every
/// control callback runs.
pub(crate) fn save_debounced(app_state: &Arc<AppState>) {
    PENDING.with(|p| p.set(true));
    let app = app_state.clone();
    TIMER.with(|t| t.start(slint::TimerMode::SingleShot, QUIET, move || write(&app)));
}

/// Write a change still waiting on the debounce.
///
/// Called after `run_event_loop_until_quit` has returned, so the pending timer
/// is left alone rather than stopped: it can no longer fire with the loop gone,
/// and this is not a moment to be poking at a half-torn-down platform.
pub(crate) fn flush(app_state: &AppState) {
    if PENDING.with(Cell::get) {
        write(app_state);
    }
}

fn write(app_state: &AppState) {
    PENDING.with(|p| p.set(false));
    let _ = app_state.with(|s, _| s.save().log_err("save settings"));
}
