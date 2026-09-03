//! Native window chrome behind the frameless UI: file drops, fullscreen,
//! maximize, minimize, always-on-top, the title-bar drag and cursor visibility.
//!
//! mpv renders into this same window's framebuffer as an OpenGL underlay (see
//! `crate::video_render`), so there is only ONE window — no separate video
//! window to position or keep in z-order.
//!
//! Every function takes the `slint::Window` it acts on, and reads the native
//! handle from it each time rather than caching one. The handle does not exist
//! until the event loop has pumped, so a cached handle is a null that every
//! caller then has to remember to check.
//!
//! The two implementations are not symmetric, and deliberately so. Windows
//! needs a WndProc subclass to keep the non-client frame from ever being
//! painted, so it drives all of this through Win32 directly. Everywhere else
//! winit already owns the frameless window, the compositor runs the move loop
//! and file drops arrive as ordinary events — so the Unix module asks winit for
//! the four things Slint does not expose and defers to Slint for the rest.
//!
//! ## Contract
//!
//! - `attach_ui` returns `false` while the native window does not exist yet,
//!   which is the caller's signal to keep trying.
//! - `set_always_on_top` returns whether the request actually took effect. It
//!   cannot on Wayland, where the protocol gives a client no way to raise
//!   itself, and the UI has to show the real state rather than a dead toggle.

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::{
    attach_ui, minimize, set_always_on_top, set_cursor_hidden, set_drop_handler, start_drag,
    toggle_fullscreen, toggle_maximize,
};

#[cfg(unix)]
mod linux;
#[cfg(unix)]
pub use linux::{
    attach_ui, minimize, set_always_on_top, set_cursor_hidden, set_drop_handler, start_drag,
    toggle_fullscreen, toggle_maximize,
};
