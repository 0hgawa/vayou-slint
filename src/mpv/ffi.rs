use std::ffi::c_void;
use std::os::raw::{c_char, c_double, c_int};
use std::sync::OnceLock;

use libloading::Library;
use tracing::info;

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
        info!("Loading libmpv");
        // We bundle libmpv-2.dll next to the exe (and a binaries/ subfolder in
        // dev). The first name that loads wins; the older soname is a fallback.
        const LIB_CANDIDATES: &[&str] = &["libmpv-2.dll", "mpv-2.dll"];

        let lib = unsafe {
            let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(std::path::Path::to_path_buf));
            let mut loaded = None;
            if let Some(dir) = exe_dir.as_ref() {
                for name in LIB_CANDIDATES {
                    if let Ok(l) = Library::new(dir.join(name)).or_else(|_| Library::new(dir.join("binaries").join(name))) {
                        loaded = Some(l);
                        break;
                    }
                }
            }
            // Fall back to the system loader (CWD / PATH).
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
                None => return Err(MpvError::LibraryLoad(format!("none of {LIB_CANDIDATES:?} could be loaded"))),
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
            info!("libmpv loaded — all symbols resolved");
            Ok(ffi)
        }
    }
}
