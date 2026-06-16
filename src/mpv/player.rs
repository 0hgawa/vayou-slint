use std::ffi::{c_void, CStr, CString};
use std::os::raw::{c_char, c_int};
use std::ptr;

use tracing::{debug, info};

use super::ffi::MpvFfi;
use super::types::{MpvEvent, MPV_FORMAT_DOUBLE, MPV_FORMAT_FLAG, MPV_FORMAT_INT64};
use crate::error::MpvError;

type MpvHandle = *mut c_void;

/// Safe wrapper around a libmpv instance.
pub struct MpvPlayer {
    handle: MpvHandle,
}

// mpv API is thread-safe: commands/properties from any thread,
// mpv_wait_event from one dedicated thread.
unsafe impl Send for MpvPlayer {}
unsafe impl Sync for MpvPlayer {}

impl MpvPlayer {
    /// Create a new mpv instance configured for the render API (`vo=libmpv`).
    /// mpv does NOT own a window — instead we drive frames into an OpenGL FBO
    /// under the Slint UI via `mpv_render_context` (see `crate::video_render`),
    /// so video and UI share a single window.
    pub fn new() -> Result<Self, MpvError> {
        let ffi = MpvFfi::init()?;

        let handle = unsafe { (ffi.create)() };
        if handle.is_null() {
            return Err(MpvError::api(-1, "mpv_create returned null"));
        }

        info!("Creating mpv instance");

        // The render API requires the embeddable `libmpv` video output, selected
        // before mpv_initialize. No `wid` — the render context owns output.
        Self::set_option_string_raw(ffi, handle, "vo", "libmpv");

        let rc = unsafe { (ffi.initialize)(handle) };
        if rc < 0 {
            return Err(MpvError::api(rc, "mpv_initialize"));
        }

        let player = Self { handle };

        // Post-init configuration. `auto-copy-safe`: hardware-decode then copy
        // frames back to system memory for GL upload. The render API runs on a
        // GLES context where mpv's zero-copy DX↔GL interop isn't available, so a
        // copy-back hwdec is the fastest path that actually works (plain HW
        // interop fails to load and a non-safe choice would fall back noisily).
        player.set::<&str>("hwdec", "auto-copy-safe")?;
        player.set::<&str>("osd-level", "0")?;
        player.set::<&str>("keep-open", "yes")?;
        player.set::<&str>("idle", "yes")?;
        player.set::<&str>("input-default-bindings", "no")?;
        player.set::<&str>("input-vo-keyboard", "no")?;
        player.set::<&str>("osc", "no")?;
        player.set::<&str>("sub-auto", "fuzzy")?;

        info!("mpv instance created and configured");

        Ok(player)
    }

    /// The raw `mpv_handle`, needed to create the render context. Valid for the
    /// player's lifetime; the render context must be freed before the player.
    pub const fn raw_handle(&self) -> *mut c_void {
        self.handle
    }

    /// Send a command to mpv (e.g. `["loadfile", "/path/to/file"]`).
    pub fn command(&self, args: &[&str]) -> Result<(), MpvError> {
        let ffi = MpvFfi::global()?;
        let c_args: Vec<CString> = args.iter().map(|s| CString::new(*s).map_err(MpvError::from)).collect::<Result<_, _>>()?;
        let mut ptrs: Vec<*const c_char> = c_args.iter().map(|s| s.as_ptr()).collect();
        ptrs.push(ptr::null());

        let rc = unsafe { (ffi.command)(self.handle, ptrs.as_ptr()) };
        if rc < 0 {
            return Err(MpvError::api(rc, &format!("command {args:?}")));
        }
        debug!(args = ?args, "mpv command");
        Ok(())
    }

    /// Observe a property for changes (delivered via `wait_event`).
    pub fn observe_property(&self, name: &str, userdata: u64, format: c_int) -> Result<(), MpvError> {
        let ffi = MpvFfi::global()?;
        let c_name = CString::new(name)?;
        let rc = unsafe { (ffi.observe_property)(self.handle, userdata, c_name.as_ptr(), format) };
        if rc < 0 {
            return Err(MpvError::api(rc, &format!("observe_property({name})")));
        }
        Ok(())
    }

    /// Block until the next event (with timeout in seconds).
    pub fn wait_event(&self, timeout: f64) -> &MpvEvent {
        let ffi = MpvFfi::global().expect("FFI must be initialized before wait_event");
        unsafe { &*(ffi.wait_event)(self.handle, timeout) }
    }

    /// Get a typed property from mpv.
    pub fn get<T: MpvProperty>(&self, name: &str) -> Result<T, MpvError> {
        T::get_from(self.handle, name)
    }

    /// Set a typed property on mpv.
    pub fn set<T: MpvProperty>(&self, name: &str, value: T) -> Result<(), MpvError> {
        T::set_on(self.handle, name, value)
    }

    /// Get a property as string (convenience for reading any property).
    pub fn get_property_string(&self, name: &str) -> Result<String, MpvError> {
        let ffi = MpvFfi::global()?;
        let c_name = CString::new(name)?;
        let ptr = unsafe { (ffi.get_property_string)(self.handle, c_name.as_ptr()) };
        if ptr.is_null() {
            return Err(MpvError::api(-1, &format!("null string for '{name}'")));
        }
        let s = unsafe { CStr::from_ptr(ptr).to_string_lossy().into_owned() };
        unsafe { (ffi.free)(ptr.cast::<c_void>()) };
        Ok(s)
    }

    /// Read a string property and parse it into `T`, falling back to `default`
    /// on null or parse failure. Covers the many numeric/`count` properties mpv
    /// only exposes as strings.
    pub fn get_num<T: std::str::FromStr>(&self, name: &str, default: T) -> T {
        self.get_property_string(name).ok().and_then(|s| s.parse().ok()).unwrap_or(default)
    }

    // --- Private helpers for pre-init options ---

    fn set_option_string_raw(ffi: &MpvFfi, handle: MpvHandle, name: &str, value: &str) {
        if let (Ok(k), Ok(v)) = (CString::new(name), CString::new(value)) {
            unsafe { (ffi.set_option_string)(handle, k.as_ptr(), v.as_ptr()) };
        }
    }
}

impl Drop for MpvPlayer {
    fn drop(&mut self) {
        if let Ok(ffi) = MpvFfi::global() {
            info!("Destroying mpv instance");
            unsafe { (ffi.terminate_destroy)(self.handle) };
        }
    }
}

// ---------------------------------------------------------------------------
// Typed property trait — compile-time type-safe mpv property access
// ---------------------------------------------------------------------------

/// Trait for types that can be read/written as mpv properties.
pub trait MpvProperty: Sized {
    fn get_from(handle: MpvHandle, name: &str) -> Result<Self, MpvError>;
    fn set_on(handle: MpvHandle, name: &str, value: Self) -> Result<(), MpvError>;
}

impl MpvProperty for f64 {
    fn get_from(handle: MpvHandle, name: &str) -> Result<Self, MpvError> {
        let ffi = MpvFfi::global()?;
        let c_name = CString::new(name)?;
        let mut val: Self = 0.0;
        let rc = unsafe { (ffi.get_property)(handle, c_name.as_ptr(), MPV_FORMAT_DOUBLE, (&raw mut val).cast::<c_void>()) };
        if rc < 0 {
            return Err(MpvError::api(rc, name));
        }
        Ok(val)
    }

    fn set_on(handle: MpvHandle, name: &str, value: Self) -> Result<(), MpvError> {
        let ffi = MpvFfi::global()?;
        let c_name = CString::new(name)?;
        let rc = unsafe { (ffi.set_property)(handle, c_name.as_ptr(), MPV_FORMAT_DOUBLE, (&raw const value).cast::<c_void>()) };
        if rc < 0 {
            return Err(MpvError::api(rc, name));
        }
        Ok(())
    }
}

impl MpvProperty for i64 {
    fn get_from(handle: MpvHandle, name: &str) -> Result<Self, MpvError> {
        let ffi = MpvFfi::global()?;
        let c_name = CString::new(name)?;
        let mut val: Self = 0;
        let rc = unsafe { (ffi.get_property)(handle, c_name.as_ptr(), MPV_FORMAT_INT64, (&raw mut val).cast::<c_void>()) };
        if rc < 0 {
            return Err(MpvError::api(rc, name));
        }
        Ok(val)
    }

    fn set_on(handle: MpvHandle, name: &str, value: Self) -> Result<(), MpvError> {
        let ffi = MpvFfi::global()?;
        let c_name = CString::new(name)?;
        let rc = unsafe { (ffi.set_property)(handle, c_name.as_ptr(), MPV_FORMAT_INT64, (&raw const value).cast::<c_void>()) };
        if rc < 0 {
            return Err(MpvError::api(rc, name));
        }
        Ok(())
    }
}

impl MpvProperty for bool {
    fn get_from(handle: MpvHandle, name: &str) -> Result<Self, MpvError> {
        let ffi = MpvFfi::global()?;
        let c_name = CString::new(name)?;
        let mut val: c_int = 0;
        let rc = unsafe { (ffi.get_property)(handle, c_name.as_ptr(), MPV_FORMAT_FLAG, (&raw mut val).cast::<c_void>()) };
        if rc < 0 {
            return Err(MpvError::api(rc, name));
        }
        Ok(val != 0)
    }

    fn set_on(handle: MpvHandle, name: &str, value: Self) -> Result<(), MpvError> {
        let ffi = MpvFfi::global()?;
        let c_name = CString::new(name)?;
        let flag: c_int = i32::from(value);
        let rc = unsafe { (ffi.set_property)(handle, c_name.as_ptr(), MPV_FORMAT_FLAG, (&raw const flag).cast::<c_void>()) };
        if rc < 0 {
            return Err(MpvError::api(rc, name));
        }
        Ok(())
    }
}

/// String properties use `set_property_string` / `get_property_string`.
impl MpvProperty for &str {
    fn get_from(_handle: MpvHandle, _name: &str) -> Result<Self, MpvError> {
        // Can't return &str from FFI — use player.get_property_string() instead.
        Err(MpvError::api(-1, "use get_property_string for string reads"))
    }

    fn set_on(handle: MpvHandle, name: &str, value: Self) -> Result<(), MpvError> {
        let ffi = MpvFfi::global()?;
        let c_name = CString::new(name)?;
        let c_value = CString::new(value)?;
        let rc = unsafe { (ffi.set_property_string)(handle, c_name.as_ptr(), c_value.as_ptr()) };
        if rc < 0 {
            return Err(MpvError::api(rc, name));
        }
        Ok(())
    }
}
