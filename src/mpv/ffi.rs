use std::ffi::c_void;
use std::os::raw::{c_char, c_double, c_int};
use std::sync::OnceLock;

use libloading::Library;

use super::types::{MpvEvent, MpvRenderContext, MpvRenderParam, MpvRenderUpdateFn};
use crate::error::MpvError;

type MpvHandle = *mut c_void;

/// All mpv function pointers, resolved once at startup. The `Library` is kept
/// alive to ensure the pointers remain valid.
pub struct MpvFfi {
    _lib: Library,
    pub create: unsafe extern "C" fn() -> MpvHandle,
    pub initialize: unsafe extern "C" fn(MpvHandle) -> c_int,
    pub terminate_destroy: unsafe extern "C" fn(MpvHandle),
    pub set_option_string: unsafe extern "C" fn(MpvHandle, *const c_char, *const c_char) -> c_int,
    pub command: unsafe extern "C" fn(MpvHandle, *const *const c_char) -> c_int,
    pub set_property: unsafe extern "C" fn(MpvHandle, *const c_char, c_int, *const c_void) -> c_int,
    pub set_property_string: unsafe extern "C" fn(MpvHandle, *const c_char, *const c_char) -> c_int,
    pub get_property: unsafe extern "C" fn(MpvHandle, *const c_char, c_int, *mut c_void) -> c_int,
    pub get_property_string: unsafe extern "C" fn(MpvHandle, *const c_char) -> *mut c_char,
    pub observe_property: unsafe extern "C" fn(MpvHandle, u64, *const c_char, c_int) -> c_int,
    pub wait_event: unsafe extern "C" fn(MpvHandle, c_double) -> *mut MpvEvent,
    pub free: unsafe extern "C" fn(*mut c_void),
    // Render API (OpenGL underlay).
    pub render_context_create: unsafe extern "C" fn(*mut MpvRenderContext, MpvHandle, *mut MpvRenderParam) -> c_int,
    pub render_context_set_update_callback: unsafe extern "C" fn(MpvRenderContext, MpvRenderUpdateFn, *mut c_void),
    // Acknowledges the update callback and re-arms it: mpv coalesces frame
    // notifications until this is called, so it must run each render.
    pub render_context_update: unsafe extern "C" fn(MpvRenderContext) -> u64,
    pub render_context_render: unsafe extern "C" fn(MpvRenderContext, *mut MpvRenderParam) -> c_int,
    pub render_context_free: unsafe extern "C" fn(MpvRenderContext),
}

// Function pointers are just addresses — safe to share across threads.
unsafe impl Send for MpvFfi {}
unsafe impl Sync for MpvFfi {}

static FFI: OnceLock<MpvFfi> = OnceLock::new();

/// What to do about a libmpv that will not load, one step per line.
///
/// Failing to find it is the one error that stops the app dead, and the fix is
/// not guessable from a list of sonames. Written once because it is read twice:
/// the window says it to the user, who is looking at a player that will not
/// play, and the log line says it to whoever reads logs. A distribution's
/// package name is exactly the sort of detail that rots when it is kept in two
/// places.
#[cfg(unix)]
pub const INSTALL_STEPS: &str =
    "Arch: pacman -S mpv\nDebian/Ubuntu: apt install libmpv2\nFedora: dnf install mpv-libs";

/// Windows ships the DLL beside the exe, so the only ways to be without it are
/// to have taken `vayou.exe` alone out of a release or to have had the DLL
/// removed. Neither is fixed by a package manager.
#[cfg(windows)]
pub const INSTALL_STEPS: &str = "Reinstall Vayou — libmpv-2.dll belongs beside vayou.exe.";

/// The libmpv filenames this build will try, in order — the first that loads
/// wins, and the older sonames are the fallback.
///
/// At module scope rather than inside `load` because `file_assoc` asks the same
/// question from the other side: whether one of these sits beside the
/// executable, which is what decides if the shell can launch this copy and have
/// it play anything.
#[cfg(windows)]
pub(crate) const LIB_CANDIDATES: &[&str] = &["libmpv-2.dll", "mpv-2.dll"];
#[cfg(unix)]
const LIB_CANDIDATES: &[&str] = &["libmpv.so.2", "libmpv.so.1", "libmpv.so"];

impl MpvFfi {
    /// Get the global FFI instance, or error if not yet initialized.
    pub fn global() -> Result<&'static Self, MpvError> {
        FFI.get().ok_or(MpvError::NotInitialized)
    }

    /// Load libmpv and resolve all symbols. Idempotent — only loads once.
    pub fn init() -> Result<&'static Self, MpvError> {
        if let Some(ffi) = FFI.get() {
            return Ok(ffi);
        }
        let ffi = Self::load()?;
        let _ = FFI.set(ffi);
        FFI.get().ok_or(MpvError::NotInitialized)
    }

    fn load() -> Result<Self, MpvError> {

        let lib = unsafe {
            let mut loaded = None;
            // Windows ships the DLL beside the exe (and in a binaries/ subfolder
            // in dev). On Unix libmpv is a distribution package that only the
            // system loader can locate, so this search would be a guaranteed
            // miss — it is compiled out rather than paying a failed open per
            // candidate on every start.
            #[cfg(windows)]
            if let Some(dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(std::path::Path::to_path_buf)) {
                for name in LIB_CANDIDATES {
                    if let Ok(l) = Library::new(dir.join(name)).or_else(|_| Library::new(dir.join("binaries").join(name))) {
                        loaded = Some(l);
                        break;
                    }
                }
            }
            // The system loader: PATH on Windows, the ld.so search path on Unix.
            if loaded.is_none() {
                for name in LIB_CANDIDATES {
                    if let Ok(l) = Library::new(name) {
                        loaded = Some(l);
                        break;
                    }
                }
            }
            match loaded {
                Some(lib) => lib,
                // The steps are newline-separated for the window; a log line is
                // one line, so they are joined for this reader only.
                None => return Err(MpvError::LibraryLoad(format!(
                    "none of {LIB_CANDIDATES:?} could be loaded — {}",
                    INSTALL_STEPS.replace('\n', "; ")
                ))),
            }
        };

        unsafe {
            let ffi = Self {
                create: *lib.get(b"mpv_create").map_err(|e| MpvError::symbol("mpv_create", e))?,
                initialize: *lib.get(b"mpv_initialize").map_err(|e| MpvError::symbol("mpv_initialize", e))?,
                terminate_destroy: *lib.get(b"mpv_terminate_destroy").map_err(|e| MpvError::symbol("mpv_terminate_destroy", e))?,
                set_option_string: *lib.get(b"mpv_set_option_string").map_err(|e| MpvError::symbol("mpv_set_option_string", e))?,
                command: *lib.get(b"mpv_command").map_err(|e| MpvError::symbol("mpv_command", e))?,
                set_property: *lib.get(b"mpv_set_property").map_err(|e| MpvError::symbol("mpv_set_property", e))?,
                set_property_string: *lib.get(b"mpv_set_property_string").map_err(|e| MpvError::symbol("mpv_set_property_string", e))?,
                get_property: *lib.get(b"mpv_get_property").map_err(|e| MpvError::symbol("mpv_get_property", e))?,
                get_property_string: *lib.get(b"mpv_get_property_string").map_err(|e| MpvError::symbol("mpv_get_property_string", e))?,
                observe_property: *lib.get(b"mpv_observe_property").map_err(|e| MpvError::symbol("mpv_observe_property", e))?,
                wait_event: *lib.get(b"mpv_wait_event").map_err(|e| MpvError::symbol("mpv_wait_event", e))?,
                free: *lib.get(b"mpv_free").map_err(|e| MpvError::symbol("mpv_free", e))?,
                render_context_create: *lib.get(b"mpv_render_context_create").map_err(|e| MpvError::symbol("mpv_render_context_create", e))?,
                render_context_set_update_callback: *lib.get(b"mpv_render_context_set_update_callback").map_err(|e| MpvError::symbol("mpv_render_context_set_update_callback", e))?,
                render_context_update: *lib.get(b"mpv_render_context_update").map_err(|e| MpvError::symbol("mpv_render_context_update", e))?,
                render_context_render: *lib.get(b"mpv_render_context_render").map_err(|e| MpvError::symbol("mpv_render_context_render", e))?,
                render_context_free: *lib.get(b"mpv_render_context_free").map_err(|e| MpvError::symbol("mpv_render_context_free", e))?,
                _lib: lib,
            };
            Ok(ffi)
        }
    }
}
