//! The OpenGL underlay: mpv renders the current video frame into the SAME
//! window the Slint UI lives in, drawn underneath it.
//!
//! Slint's femtovg renderer owns the window's GL context. In the
//! `BeforeRendering` notifier the context is current and the back buffer is
//! bound, so mpv's render API (`mpv_render_context`, `vo=libmpv`) draws the
//! frame into that framebuffer; Slint then paints the (transparent-where-video)
//! UI on top. One window, one swapchain — no second HWND, no z-order syncing.
//!
//! Slint requires the GL state to be preserved across the notifier
//! (see `slint::RenderingState` docs), so every mpv render is bracketed by a
//! focused save/restore of the state femtovg depends on.

use std::cell::{Cell, OnceCell, RefCell};
use std::ffi::{c_void, CStr, CString};
use std::os::raw::c_char;
use std::path::PathBuf;
use std::ptr;
use std::sync::Arc;

use libloading::Library;
use slint::{GraphicsAPI, RenderingState, SetRenderingNotifierError};

use crate::mpv::ffi::MpvFfi;
use crate::mpv::types::{
    MpvOpenGLFbo, MpvOpenGLInitParams, MpvRenderContext, MpvRenderParam,
    MPV_RENDER_API_TYPE_OPENGL, MPV_RENDER_PARAM_API_TYPE, MPV_RENDER_PARAM_FLIP_Y,
    MPV_RENDER_PARAM_INVALID, MPV_RENDER_PARAM_OPENGL_FBO, MPV_RENDER_PARAM_OPENGL_INIT_PARAMS,
};
use crate::state::MpvState;

// --- GL enums we query/restore (only what femtovg can leak). ---
const GL_FRAMEBUFFER: u32 = 0x8D40;
const GL_FRAMEBUFFER_BINDING: u32 = 0x8CA6;
const GL_VIEWPORT: u32 = 0x0BA2;
const GL_CURRENT_PROGRAM: u32 = 0x8B8D;
const GL_ARRAY_BUFFER: u32 = 0x8892;
const GL_ARRAY_BUFFER_BINDING: u32 = 0x8894;
const GL_VERTEX_ARRAY_BINDING: u32 = 0x85B5;
const GL_TEXTURE0: u32 = 0x84C0;
const GL_ACTIVE_TEXTURE: u32 = 0x84E0;
const GL_TEXTURE_2D: u32 = 0x0DE1;
const GL_TEXTURE_BINDING_2D: u32 = 0x8069;
const GL_BLEND: u32 = 0x0BE2;
const GL_BLEND_SRC_RGB: u32 = 0x80C9;
const GL_BLEND_DST_RGB: u32 = 0x80C8;
const GL_BLEND_SRC_ALPHA: u32 = 0x80CB;
const GL_BLEND_DST_ALPHA: u32 = 0x80CA;
const GL_BLEND_EQUATION_RGB: u32 = 0x8009;
const GL_BLEND_EQUATION_ALPHA: u32 = 0x883D;
const GL_SCISSOR_TEST: u32 = 0x0C11;
const GL_SCISSOR_BOX: u32 = 0x0C10;
const GL_UNPACK_ALIGNMENT: u32 = 0x0CF5;
const GL_RGBA: u32 = 0x1908;
const GL_UNSIGNED_BYTE: u32 = 0x1401;

type GlGetIntegerv = unsafe extern "C" fn(u32, *mut i32);
type GlBindFramebuffer = unsafe extern "C" fn(u32, u32);
type GlViewport = unsafe extern "C" fn(i32, i32, i32, i32);
type GlUseProgram = unsafe extern "C" fn(u32);
type GlBindBuffer = unsafe extern "C" fn(u32, u32);
type GlBindVertexArray = unsafe extern "C" fn(u32);
type GlActiveTexture = unsafe extern "C" fn(u32);
type GlBindTexture = unsafe extern "C" fn(u32, u32);
type GlCap = unsafe extern "C" fn(u32);
type GlIsEnabled = unsafe extern "C" fn(u32) -> u8;
type GlBlendFuncSeparate = unsafe extern "C" fn(u32, u32, u32, u32);
type GlBlendEquationSeparate = unsafe extern "C" fn(u32, u32);
type GlScissor = unsafe extern "C" fn(i32, i32, i32, i32);
type GlPixelStorei = unsafe extern "C" fn(u32, i32);
type GlReadPixels = unsafe extern "C" fn(i32, i32, i32, i32, u32, u32, *mut c_void);

/// The system GL library, opened only as a name fallback (see `GlFns::load`).
#[cfg(windows)]
const GL_LIB: &str = "opengl32.dll";
#[cfg(unix)]
const GL_LIB: &str = "libGL.so.1";

/// The display-server connection handed to `mpv_render_context_create`: the
/// render-parameter type mpv expects, paired with the pointer itself.
///
/// A pair rather than a new enum, because the pair is exactly what goes into
/// the parameter array — and it can be `None` on a platform with nothing to
/// pass without leaving an uninhabited type behind.
pub type NativeDisplay = (std::os::raw::c_int, *mut c_void);

/// Read the display-server connection behind a Slint window, for mpv's
/// hardware-decode interop.
///
/// Must be called after the native window exists — the caller does so from the
/// rendering notifier, which only runs once it does.
#[cfg(unix)]
pub fn native_display(win: &slint::Window) -> Option<NativeDisplay> {
    use crate::mpv::types::{MPV_RENDER_PARAM_WL_DISPLAY, MPV_RENDER_PARAM_X11_DISPLAY};
    use raw_window_handle::{HasDisplayHandle, RawDisplayHandle};

    let slint_handle = win.window_handle();
    let display = slint_handle.display_handle().ok()?;
    match display.as_raw() {
        RawDisplayHandle::Xlib(x) => Some((MPV_RENDER_PARAM_X11_DISPLAY, x.display?.as_ptr())),
        RawDisplayHandle::Wayland(w) => Some((MPV_RENDER_PARAM_WL_DISPLAY, w.display.as_ptr())),
        // An XCB connection is not the Xlib `Display*` mpv's VAAPI interop asks
        // for. Better to pass nothing — and decode in software — than to hand
        // mpv a pointer of the wrong type.
        _ => None,
    }
}

/// Windows has no display connection to pass: there the GL context alone
/// carries everything mpv's interop needs.
#[cfg(windows)]
pub const fn native_display(_win: &slint::Window) -> Option<NativeDisplay> {
    None
}

/// The handful of GL entry points used to query the render target and to
/// save/restore state around mpv. Resolved via Slint's `get_proc_address`, with
/// a fallback to the system GL library for the GL 1.1 core functions that
/// `wglGetProcAddress` refuses to return.
///
/// That fallback is opened lazily, on the first symbol that actually misses.
/// GLX and EGL return the GL 1.1 entry points normally, so on Linux the common
/// path never pays for the `dlopen` at all — and the safety net is still there,
/// because an EGL older than 1.5 is not required to return them.
struct GlFns {
    _gl_lib: Option<Library>,
    get_integerv: Option<GlGetIntegerv>,
    bind_framebuffer: Option<GlBindFramebuffer>,
    viewport: Option<GlViewport>,
    use_program: Option<GlUseProgram>,
    bind_buffer: Option<GlBindBuffer>,
    bind_vertex_array: Option<GlBindVertexArray>,
    active_texture: Option<GlActiveTexture>,
    bind_texture: Option<GlBindTexture>,
    enable: Option<GlCap>,
    disable: Option<GlCap>,
    is_enabled: Option<GlIsEnabled>,
    blend_func_separate: Option<GlBlendFuncSeparate>,
    blend_equation_separate: Option<GlBlendEquationSeparate>,
    scissor: Option<GlScissor>,
    pixel_storei: Option<GlPixelStorei>,
    read_pixels: Option<GlReadPixels>,
}

const unsafe fn as_fn<F>(p: *const c_void) -> Option<F> {
    if p.is_null() {
        None
    } else {
        // SAFETY: caller guarantees `p` is a valid entry point of type `F`
        // (a function pointer, same size as the data pointer we copy from).
        Some(unsafe { std::mem::transmute_copy::<*const c_void, F>(&p) })
    }
}

impl GlFns {
    fn load(get_proc_address: &dyn Fn(&CStr) -> *const c_void) -> Self {
        // Opened on the first miss and never again — see the struct's docs.
        let gl_lib: OnceCell<Option<Library>> = OnceCell::new();
        let resolve = |name: &str| -> *const c_void {
            let Ok(cname) = CString::new(name) else { return ptr::null() };
            let p = get_proc_address(&cname);
            if !p.is_null() {
                return p;
            }
            // Fallback for GL 1.1 core functions (glViewport, glBindTexture, …).
            // SAFETY: loading the system GL library; only used as a name lookup.
            let lib = gl_lib.get_or_init(|| unsafe { Library::new(GL_LIB).ok() });
            if let Some(lib) = lib.as_ref() {
                let mut bytes = name.as_bytes().to_vec();
                bytes.push(0);
                // SAFETY: `bytes` is NUL-terminated; we read the symbol address.
                unsafe {
                    if let Ok(sym) = lib.get::<unsafe extern "C" fn()>(&bytes) {
                        return *sym as usize as *const c_void;
                    }
                }
            }
            ptr::null()
        };
        // SAFETY: each name maps to the matching GL prototype in `as_fn`.
        unsafe {
            Self {
                get_integerv: as_fn(resolve("glGetIntegerv")),
                bind_framebuffer: as_fn(resolve("glBindFramebuffer")),
                viewport: as_fn(resolve("glViewport")),
                use_program: as_fn(resolve("glUseProgram")),
                bind_buffer: as_fn(resolve("glBindBuffer")),
                bind_vertex_array: as_fn(resolve("glBindVertexArray")),
                active_texture: as_fn(resolve("glActiveTexture")),
                bind_texture: as_fn(resolve("glBindTexture")),
                enable: as_fn(resolve("glEnable")),
                disable: as_fn(resolve("glDisable")),
                is_enabled: as_fn(resolve("glIsEnabled")),
                blend_func_separate: as_fn(resolve("glBlendFuncSeparate")),
                blend_equation_separate: as_fn(resolve("glBlendEquationSeparate")),
                scissor: as_fn(resolve("glScissor")),
                pixel_storei: as_fn(resolve("glPixelStorei")),
                read_pixels: as_fn(resolve("glReadPixels")),
                // Keep whatever the fallback opened alive for the resolved
                // pointers' lifetime; `None` when nothing ever missed.
                _gl_lib: gl_lib.into_inner().flatten(),
            }
        }
    }

    fn get1(&self, pname: u32) -> i32 {
        let mut v = 0;
        if let Some(f) = self.get_integerv {
            // SAFETY: `pname` returns a single integer; `v` is a live i32.
            unsafe { f(pname, &raw mut v) };
        }
        v
    }

    fn get4(&self, pname: u32) -> [i32; 4] {
        let mut v = [0i32; 4];
        if let Some(f) = self.get_integerv {
            // SAFETY: `pname` returns 4 integers; `v` holds 4.
            unsafe { f(pname, v.as_mut_ptr()) };
        }
        v
    }

    fn is_on(&self, cap: u32) -> bool {
        // SAFETY: standard glIsEnabled query.
        self.is_enabled.is_some_and(|f| unsafe { f(cap) } != 0)
    }
}

/// A snapshot of the GL state femtovg relies on, taken before mpv renders and
/// restored after.
struct GlState {
    fbo: i32,
    viewport: [i32; 4],
    program: i32,
    array_buffer: i32,
    vertex_array: i32,
    active_texture: i32,
    texture_2d: i32,
    blend: bool,
    blend_func: [i32; 4],
    blend_eq: [i32; 2],
    scissor: bool,
    scissor_box: [i32; 4],
    unpack_alignment: i32,
}

impl GlState {
    fn save(gl: &GlFns) -> Self {
        Self {
            fbo: gl.get1(GL_FRAMEBUFFER_BINDING),
            viewport: gl.get4(GL_VIEWPORT),
            program: gl.get1(GL_CURRENT_PROGRAM),
            array_buffer: gl.get1(GL_ARRAY_BUFFER_BINDING),
            vertex_array: gl.get1(GL_VERTEX_ARRAY_BINDING),
            active_texture: gl.get1(GL_ACTIVE_TEXTURE),
            texture_2d: gl.get1(GL_TEXTURE_BINDING_2D),
            blend: gl.is_on(GL_BLEND),
            blend_func: [
                gl.get1(GL_BLEND_SRC_RGB), gl.get1(GL_BLEND_DST_RGB),
                gl.get1(GL_BLEND_SRC_ALPHA), gl.get1(GL_BLEND_DST_ALPHA),
            ],
            blend_eq: [gl.get1(GL_BLEND_EQUATION_RGB), gl.get1(GL_BLEND_EQUATION_ALPHA)],
            scissor: gl.is_on(GL_SCISSOR_TEST),
            scissor_box: gl.get4(GL_SCISSOR_BOX),
            unpack_alignment: gl.get1(GL_UNPACK_ALIGNMENT),
        }
    }

    #[allow(clippy::cognitive_complexity)]
    fn restore(&self, gl: &GlFns) {
        // SAFETY: every call replays a value we read with the matching getter,
        // so the arguments are valid for this context.
        unsafe {
            if let Some(f) = gl.bind_framebuffer { f(GL_FRAMEBUFFER, self.fbo as u32); }
            if let Some(f) = gl.viewport { f(self.viewport[0], self.viewport[1], self.viewport[2], self.viewport[3]); }
            if let Some(f) = gl.use_program { f(self.program as u32); }
            if let Some(f) = gl.bind_vertex_array { f(self.vertex_array as u32); }
            if let Some(f) = gl.bind_buffer { f(GL_ARRAY_BUFFER, self.array_buffer as u32); }
            if let (Some(at), Some(bt)) = (gl.active_texture, gl.bind_texture) {
                at(self.active_texture as u32);
                bt(GL_TEXTURE_2D, self.texture_2d as u32);
            }
            if let Some(f) = gl.blend_func_separate {
                f(self.blend_func[0] as u32, self.blend_func[1] as u32, self.blend_func[2] as u32, self.blend_func[3] as u32);
            }
            if let Some(f) = gl.blend_equation_separate { f(self.blend_eq[0] as u32, self.blend_eq[1] as u32); }
            if let (Some(en), Some(dis)) = (gl.enable, gl.disable) {
                if self.blend { en(GL_BLEND); } else { dis(GL_BLEND); }
                if self.scissor { en(GL_SCISSOR_TEST); } else { dis(GL_SCISSOR_TEST); }
            }
            if let Some(f) = gl.scissor { f(self.scissor_box[0], self.scissor_box[1], self.scissor_box[2], self.scissor_box[3]); }
            if let Some(f) = gl.pixel_storei { f(GL_UNPACK_ALIGNMENT, self.unpack_alignment); }
            // Restore the active texture unit last so it isn't left on GL_TEXTURE0.
            if let Some(f) = gl.active_texture { f(self.active_texture.max(GL_TEXTURE0 as i32) as u32); }
        }
    }
}

/// The mpv render context plus the GL hooks needed to drive it. Lives on the
/// UI/render thread (holds raw GL state — not `Send`).
pub struct RenderContext {
    ctx: MpvRenderContext,
    gl: GlFns,
    // The boxed update closure whose address was handed to mpv; kept alive here
    // so the pointer stays valid until the context is freed.
    _update_cb: Box<UpdateCb>,
}

type UpdateCb = Box<dyn Fn() + Send + Sync>;

/// Trampoline mpv calls (during context creation) to resolve GL functions.
unsafe extern "C" fn get_proc_address_trampoline(ctx: *mut c_void, name: *const c_char) -> *mut c_void {
    // SAFETY: `ctx` is the `&dyn Fn` we passed in `create`, valid for the call.
    let f = unsafe { &*(ctx as *const &dyn Fn(&CStr) -> *const c_void) };
    // SAFETY: mpv passes a valid NUL-terminated C string.
    let cname = unsafe { CStr::from_ptr(name) };
    f(cname).cast_mut()
}

/// Trampoline mpv calls (off-thread) when a new frame is ready.
unsafe extern "C" fn update_trampoline(ctx: *mut c_void) {
    // SAFETY: `ctx` is the `&UpdateCb` whose box `RenderContext` keeps alive.
    let f = unsafe { &*(ctx as *const UpdateCb) };
    f();
}

impl RenderContext {
    /// Create the render context against the live GL context. Must be called on
    /// the render thread with the context current (i.e. inside the notifier).
    ///
    /// # Safety
    /// `handle` must be a live, initialized `mpv_handle`, and a GL context must
    /// be current on the calling thread.
    unsafe fn create(
        handle: *mut c_void,
        get_proc_address: &dyn Fn(&CStr) -> *const c_void,
        display: Option<NativeDisplay>,
        update_cb: UpdateCb,
    ) -> Result<Self, String> {
        let ffi = MpvFfi::global().map_err(|e| e.to_string())?;
        let gl = GlFns::load(get_proc_address);

        let gpa_ref: &dyn Fn(&CStr) -> *const c_void = get_proc_address;
        let mut init = MpvOpenGLInitParams {
            get_proc_address: get_proc_address_trampoline,
            get_proc_address_ctx: (&raw const gpa_ref).cast::<c_void>().cast_mut(),
        };
        let mut params = Vec::with_capacity(4);
        params.push(MpvRenderParam {
            type_: MPV_RENDER_PARAM_API_TYPE,
            data: MPV_RENDER_API_TYPE_OPENGL.as_ptr().cast::<c_void>().cast_mut(),
        });
        params.push(MpvRenderParam {
            type_: MPV_RENDER_PARAM_OPENGL_INIT_PARAMS,
            data: (&raw mut init).cast::<c_void>(),
        });
        // The display-server connection, where the platform has one to give.
        // This is what lets libmpv open a VA display and use the GPU decoder:
        // without it hardware decoding is disabled silently, and the only
        // symptom is a CPU pegged by software decoding.
        if let Some((param_type, ptr)) = display {
            params.push(MpvRenderParam { type_: param_type, data: ptr });
        }
        params.push(MpvRenderParam { type_: MPV_RENDER_PARAM_INVALID, data: ptr::null_mut() });

        let mut ctx: MpvRenderContext = ptr::null_mut();
        // SAFETY: `params` (and `init`/`gpa_ref` it points at) outlive the call.
        let rc = unsafe { (ffi.render_context_create)(&raw mut ctx, handle, params.as_mut_ptr()) };
        if rc < 0 || ctx.is_null() {
            return Err(format!("mpv_render_context_create failed ({rc})"));
        }

        // Register the frame-ready callback. Its box is stored below so the
        // pointer handed to mpv stays valid until the context is freed.
        let update_cb: Box<UpdateCb> = Box::new(update_cb);
        let cb_ptr = (&raw const *update_cb).cast::<c_void>().cast_mut();
        // SAFETY: `ctx` is live; `cb_ptr` outlives it (moved into the struct).
        unsafe { (ffi.render_context_set_update_callback)(ctx, update_trampoline, cb_ptr) };

        Ok(Self { ctx, gl, _update_cb: update_cb })
    }

    /// Draw the current mpv frame into the framebuffer Slint is about to render
    /// into, preserving the GL state femtovg depends on.
    fn render_underlay(&self) {
        let Ok(ffi) = MpvFfi::global() else { return };
        // Acknowledge/re-arm the update callback so mpv keeps notifying us of new
        // frames (it coalesces until this is called). We repaint every frame
        // regardless — femtovg clears the buffer each pass — so the returned
        // flags aren't needed.
        // SAFETY: `self.ctx` is a live render context.
        unsafe { (ffi.render_context_update)(self.ctx) };

        let fbo = self.gl.get1(GL_FRAMEBUFFER_BINDING);
        let vp = self.gl.get4(GL_VIEWPORT);
        let (w, h) = (vp[2], vp[3]);
        if w <= 0 || h <= 0 {
            return;
        }
        let saved = GlState::save(&self.gl);

        // Neutralize the state femtovg leaves that would otherwise clip mpv's
        // output: a live scissor box clips the video to nothing (→ black), and a
        // leftover blend func would composite the opaque frame wrongly.
        // SAFETY: plain state setters on the current context.
        unsafe {
            if let Some(f) = self.gl.disable {
                f(GL_SCISSOR_TEST);
                f(GL_BLEND);
            }
            if let Some(f) = self.gl.viewport {
                f(0, 0, w, h);
            }
        }

        let mut fbo_param = MpvOpenGLFbo { fbo, w, h, internal_format: 0 };
        let mut flip: i32 = 1;
        let mut params = [
            MpvRenderParam { type_: MPV_RENDER_PARAM_OPENGL_FBO, data: (&raw mut fbo_param).cast::<c_void>() },
            MpvRenderParam { type_: MPV_RENDER_PARAM_FLIP_Y, data: (&raw mut flip).cast::<c_void>() },
            MpvRenderParam { type_: MPV_RENDER_PARAM_INVALID, data: ptr::null_mut() },
        ];
        // SAFETY: context is current; `params` outlive the call.
        unsafe { (ffi.render_context_render)(self.ctx, params.as_mut_ptr()) };

        // Here and nowhere else: mpv has drawn the frame and its subtitles, and
        // Slint has not yet drawn the controls over them. One step later the
        // buffer holds a picture with a seek bar across it.
        self.serve_screenshot_request(w, h);

        saved.restore(&self.gl);
    }

    /// Write the frame just rendered to whatever path was asked for, if any.
    fn serve_screenshot_request(&self, w: i32, h: i32) {
        let Some((path, done)) = PENDING_SHOT.with(|p| p.borrow_mut().take()) else { return };
        done(self.read_framebuffer(w, h).and_then(|rgba| write_png(&path, &rgba, w, h)));
    }

    /// Read the bound framebuffer back as RGBA, top row first.
    ///
    /// No `glPixelStorei` call is needed to get there: a row is `w * 4` bytes,
    /// always a multiple of GL's default four-byte pack alignment, so nothing
    /// is padded and no state has to be borrowed from femtovg and put back.
    fn read_framebuffer(&self, w: i32, h: i32) -> Result<Vec<u8>, String> {
        let read = self.gl.read_pixels.ok_or("glReadPixels is unavailable")?;
        let stride = w as usize * 4;
        let mut rgba = vec![0u8; stride * h as usize];
        // SAFETY: the buffer is exactly the w*h RGBA bytes this region asks for.
        unsafe { read(0, 0, w, h, GL_RGBA, GL_UNSIGNED_BYTE, rgba.as_mut_ptr().cast()) };

        // GL numbers rows from the bottom of the image, PNG from the top. An odd
        // height leaves its middle row where it already belongs.
        let (top, bottom) = rgba.split_at_mut(stride * (h as usize / 2));
        for (a, b) in top.chunks_exact_mut(stride).zip(bottom.chunks_exact_mut(stride).rev()) {
            a.swap_with_slice(b);
        }
        Ok(rgba)
    }
}

// Set on the way out, when the context is abandoned instead of freed — see
// `bridge::window::close_app` for why.
thread_local! {
    static ABANDON_CONTEXT: Cell<bool> = const { Cell::new(false) };
}

/// Stop freeing the render context when the window goes. One-way: nothing
/// clears it, because nothing happens after it but the process exiting.
pub fn abandon_context_on_teardown() {
    ABANDON_CONTEXT.with(|f| f.set(true));
}

/// A screenshot waiting on the next frame: the capture can only happen inside
/// the rendering notifier, so a request made from a keypress is parked here.
/// UI thread only, which is where both of those run.
type ShotDone = Box<dyn FnOnce(Result<(), String>)>;
thread_local! {
    static PENDING_SHOT: RefCell<Option<(PathBuf, ShotDone)>> = const { RefCell::new(None) };
}

/// Ask for the next rendered frame to be written to `path`; `on_done` runs on
/// the UI thread once it has been, or once it is clear it cannot be.
///
/// The caller must also ask for a redraw: paused there is no next frame, and
/// the request would sit here until something else repainted the window.
pub fn request_screenshot(path: PathBuf, on_done: impl FnOnce(Result<(), String>) + 'static) {
    PENDING_SHOT.with(|p| *p.borrow_mut() = Some((path, Box::new(on_done))));
}

/// Write RGBA pixels out as a PNG.
fn write_png(path: &std::path::Path, rgba: &[u8], w: i32, h: i32) -> Result<(), String> {
    let file = std::fs::File::create(path).map_err(|e| format!("create file: {e}"))?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w as u32, h as u32);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()
        .map_err(|e| format!("write header: {e}"))?
        .write_image_data(rgba)
        .map_err(|e| format!("write image: {e}"))
}

impl Drop for RenderContext {
    fn drop(&mut self) {
        if let Ok(ffi) = MpvFfi::global() {
            // SAFETY: frees the context; stops any further update callbacks
            // before the boxed closure is dropped.
            unsafe { (ffi.render_context_free)(self.ctx) };
        }
    }
}

/// Install the rendering notifier that drives the mpv underlay. The render
/// context is created lazily on the first frame after mpv is initialized;
/// `request_redraw` is what mpv's update callback calls (off-thread) to ask the
/// UI thread for another frame.
/// `on_ready` fires once, right after the render context is first created — the
/// moment mpv's `vo=libmpv` becomes usable. The first file must not be loaded
/// before this, or mpv finds "no render context", drops the video track, and
/// plays audio-only.
///
/// `native_display` is asked for the display-server connection at that same
/// moment. It is a closure rather than a value because the native window does
/// not exist yet when this is called — only by the time the notifier first runs.
pub fn install(
    window: &slint::Window,
    mpv_state: Arc<MpvState>,
    request_redraw: impl Fn() + Send + Sync + 'static,
    on_ready: impl FnOnce() + 'static,
    native_display: impl Fn() -> Option<NativeDisplay> + 'static,
) -> Result<(), SetRenderingNotifierError> {
    let request_redraw: Arc<dyn Fn() + Send + Sync> = Arc::new(request_redraw);
    let ctx: RefCell<Option<RenderContext>> = RefCell::new(None);
    let on_ready: RefCell<Option<Box<dyn FnOnce()>>> = RefCell::new(Some(Box::new(on_ready)));

    window.set_rendering_notifier(move |state, api| {
        let GraphicsAPI::NativeOpenGL { get_proc_address } = api else { return };
        match state {
            RenderingState::BeforeRendering => {
                let mut slot = ctx.borrow_mut();
                if slot.is_none() {
                    if let Ok(mpv) = mpv_state.get() {
                        let rr = request_redraw.clone();
                        // SAFETY: in the notifier the GL context is current and
                        // the mpv handle is a live, initialized instance.
                        let display = native_display();
                        match unsafe { RenderContext::create(mpv.raw_handle(), get_proc_address, display, Box::new(move || rr())) } {
                            Ok(rc) => {
                                *slot = Some(rc);
                                if let Some(cb) = on_ready.borrow_mut().take() {
                                    cb();
                                }
                            }
                            Err(e) => tracing::error!(error = %e, "render context creation failed"),
                        }
                    }
                }
                if let Some(rc) = slot.as_ref() {
                    rc.render_underlay();
                }
            }
            RenderingState::RenderingTeardown => {
                let taken = ctx.borrow_mut().take();
                if ABANDON_CONTEXT.with(Cell::get) {
                    // On the way out, and deliberately: see
                    // `abandon_context_on_teardown`.
                    std::mem::forget(taken);
                } else {
                    drop(taken);
                }
            }
            _ => {}
        }
    })
}
