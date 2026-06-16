use std::ffi::c_void;
use std::os::raw::{c_char, c_int};

/// mpv property format identifiers.
pub const MPV_FORMAT_STRING: c_int = 1;
pub const MPV_FORMAT_FLAG: c_int = 3;
pub const MPV_FORMAT_INT64: c_int = 4;
pub const MPV_FORMAT_DOUBLE: c_int = 5;

/// mpv event identifiers (from client.h's `mpv_event_id`).
pub const MPV_EVENT_SHUTDOWN: c_int = 1;
pub const MPV_EVENT_END_FILE: c_int = 7;
pub const MPV_EVENT_FILE_LOADED: c_int = 8;
pub const MPV_EVENT_PROPERTY_CHANGE: c_int = 22;

/// Raw mpv event as returned by `mpv_wait_event`.
#[repr(C)]
pub struct MpvEvent {
    pub event_id: c_int,
    pub error: c_int,
    pub reply_userdata: u64,
    pub data: *mut c_void,
}

/// Property change data within an mpv event.
#[repr(C)]
pub struct MpvEventProperty {
    pub name: *const c_char,
    pub format: c_int,
    pub data: *mut c_void,
}

// ---------------------------------------------------------------------------
// Render API (render.h / render_gl.h) — the OpenGL underlay path.
//
// mpv renders the current frame into an OpenGL framebuffer we own, inside
// Slint's `BeforeRendering` notifier, so the video sits under the UI in the
// SAME window (no second HWND). ABI-stable since libmpv's render API landed.
// ---------------------------------------------------------------------------

/// Opaque render-context handle (`mpv_render_context*`).
pub type MpvRenderContext = *mut c_void;

/// `mpv_render_param_type` — only the values we use.
pub const MPV_RENDER_PARAM_INVALID: c_int = 0;
pub const MPV_RENDER_PARAM_API_TYPE: c_int = 1;
pub const MPV_RENDER_PARAM_OPENGL_INIT_PARAMS: c_int = 2;
pub const MPV_RENDER_PARAM_OPENGL_FBO: c_int = 3;
pub const MPV_RENDER_PARAM_FLIP_Y: c_int = 4;

/// `MPV_RENDER_API_TYPE_OPENGL` — the NUL-terminated API-type string.
pub const MPV_RENDER_API_TYPE_OPENGL: &[u8] = b"opengl\0";

/// One entry of the NUL-terminated (`type == 0`) render-param array.
#[repr(C)]
pub struct MpvRenderParam {
    pub type_: c_int,
    pub data: *mut c_void,
}

/// `mpv_opengl_init_params` — how mpv resolves GL functions at context creation.
#[repr(C)]
pub struct MpvOpenGLInitParams {
    pub get_proc_address: unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_void,
    pub get_proc_address_ctx: *mut c_void,
}

/// `mpv_opengl_fbo` — the target framebuffer mpv draws into.
#[repr(C)]
pub struct MpvOpenGLFbo {
    pub fbo: c_int,
    pub w: c_int,
    pub h: c_int,
    pub internal_format: c_int,
}

/// The update-callback signature mpv calls when a new frame is available.
pub type MpvRenderUpdateFn = unsafe extern "C" fn(*mut c_void);
