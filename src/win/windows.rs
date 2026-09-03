//! Windows implementation of the window chrome described in the parent module:
//! app icon, drag-and-drop (WM_DROPFILES), fullscreen / maximize /
//! always-on-top / minimize, and the frameless chrome.
//!
//! All of it goes through Win32 rather than winit, because the non-client frame
//! has to be suppressed by a WndProc subclass — winit's femtovg backend
//! re-decorates the window on activation, and a native caption flashes in.
//!
//! The window state that has no Win32 query (`WIN_STATE`, the pre-fullscreen
//! rect) stays in statics here. The HWND does not: it is read from the
//! `slint::Window` each call, per the parent module's contract.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU8, Ordering};
use std::sync::Mutex;

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{BOOL, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, TRUE, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_BORDER_COLOR, DWMWA_COLOR_DEFAULT, DWMWA_COLOR_NONE,
    DWMWA_TRANSITIONS_FORCEDISABLED, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_DONOTROUND,
    DWMWCP_ROUND, DWM_WINDOW_CORNER_PREFERENCE,
};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromWindow, ScreenToClient, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{DragAcceptFiles, DragFinish, DragQueryFileW, HDROP};
use windows::Win32::UI::Input::KeyboardAndMouse::ReleaseCapture;
use windows::Win32::UI::WindowsAndMessaging::{
    CallWindowProcW,
    GetCursorPos, GetSystemMetrics, GetWindowLongPtrW, GetWindowRect, LoadImageW, PostMessageW,
    SendMessageW, SetClassLongPtrW, SetWindowLongPtrW, SetWindowPos, ShowCursor, ShowWindow, GCLP_HICON, GCLP_HICONSM,
    GWLP_WNDPROC, GWL_STYLE, HTCAPTION, HWND_NOTOPMOST, HWND_TOPMOST, IMAGE_ICON, LR_DEFAULTCOLOR,
    LR_SHARED, SM_CXICON, SM_CXSMICON, SM_CYICON, SM_CYSMICON, SWP_NOACTIVATE, SWP_FRAMECHANGED,
    SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SW_MINIMIZE,
    WM_LBUTTONUP, WM_NCLBUTTONDOWN, WS_CAPTION,
};

type WndProcFn = unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT;

static ORIG_WNDPROC: AtomicIsize = AtomicIsize::new(0);

// Window state: 0 = normal, 1 = maximized (work area), 2 = fullscreen (monitor).
const NORMAL: u8 = 0;
const MAXIMIZED: u8 = 1;
const FULLSCREEN: u8 = 2;
static WIN_STATE: AtomicU8 = AtomicU8::new(NORMAL);
static ALWAYS_ON_TOP: AtomicBool = AtomicBool::new(false);
static SAVED_RECT: Mutex<Option<RECT>> = Mutex::new(None);
static CURSOR_HIDDEN: AtomicBool = AtomicBool::new(false);

const WM_DROPFILES: u32 = 0x0233;
const WM_NCCALCSIZE: u32 = 0x0083;
const WM_NCPAINT: u32 = 0x0085;
const WM_NCACTIVATE: u32 = 0x0086;

thread_local! {
    static DROP_HANDLER: RefCell<Option<Box<dyn Fn(String)>>> = const { RefCell::new(None) };
}

/// The HWND behind a Slint window, via the raw-window-handle bridge. `None`
/// until the event loop has created the native window.
fn hwnd_of(win: &slint::Window) -> Option<HWND> {
    let slint_handle = win.window_handle();
    let handle = slint_handle.window_handle().ok()?;
    match handle.as_raw() {
        RawWindowHandle::Win32(raw) => Some(HWND(raw.hwnd.get() as *mut core::ffi::c_void)),
        _ => None,
    }
}

/// Register a callback for files dropped onto the window (set from `main`).
pub fn set_drop_handler(f: impl Fn(String) + 'static) {
    DROP_HANDLER.with(|h| *h.borrow_mut() = Some(Box::new(f)));
}

/// Install the brand icon on winit's window class (once).
fn ensure_app_icon(hwnd: HWND) {
    // SAFETY: module handle is this exe; LoadImageW reads its embedded icon.
    unsafe {
        let Ok(hmod) = GetModuleHandleW(None) else { return };
        let hinst = HINSTANCE(hmod.0);
        let load = |cx, cy| LoadImageW(Some(hinst), PCWSTR(1 as _), IMAGE_ICON, cx, cy, LR_DEFAULTCOLOR | LR_SHARED).ok();
        if let Some(big) = load(GetSystemMetrics(SM_CXICON), GetSystemMetrics(SM_CYICON)) {
            SetClassLongPtrW(hwnd, GCLP_HICON, big.0 as isize);
        }
        if let Some(small) = load(GetSystemMetrics(SM_CXSMICON), GetSystemMetrics(SM_CYSMICON)) {
            SetClassLongPtrW(hwnd, GCLP_HICONSM, small.0 as isize);
        }
    }
}

/// Strip the native title bar. winit's `no-frame` window is occasionally
/// created decorated under the femtovg backend (a classic caption appears); we
/// force it off, keeping the sizing border (WS_THICKFRAME) for resizing.
fn enforce_frameless(hwnd: HWND) {
    // SAFETY: `hwnd` valid; we only clear WS_CAPTION and refresh the frame.
    unsafe {
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE);
        let caption = i64::from(WS_CAPTION.0) as isize;
        if style & caption != 0 {
            SetWindowLongPtrW(hwnd, GWL_STYLE, style & !caption);
            let _ = SetWindowPos(hwnd, None, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED);
        }
    }
}

fn disable_dwm_transitions(hwnd: HWND) {
    let yes: BOOL = TRUE;
    // SAFETY: `hwnd` valid; the attribute pointer is a live BOOL.
    unsafe {
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_TRANSITIONS_FORCEDISABLED, std::ptr::addr_of!(yes).cast(), core::mem::size_of::<BOOL>() as u32);
    }
}

/// Ask Windows 11 for its rounded corners — but only while the window floats.
///
/// Dropping WS_CAPTION in `enforce_frameless` is what costs them: the compositor
/// rounds a window with a title bar on its own, and a frameless one has to ask.
/// The Tauri build gets the same look from CSS instead, which it has no choice
/// about — a WebView cannot clip itself, so that window is transparent and the
/// page draws the corners. Here mpv owns the back buffer, so the same trick
/// would leave video in the corners and cost the direct-scanout path; the
/// compositor clips after composition, whoever painted the pixel.
///
/// The attribute is sticky, and that is the whole reason this takes the state
/// rather than being set once at startup. Windows rounds a *floating* window;
/// maximized and fullscreen ones sit flush against the screen edge with square
/// corners, and the shell applies that rule to a titled window by itself. A
/// frameless window that asked once keeps the rounding — so a film played
/// fullscreen was shown with four clipped corners and DWM's border stroked
/// around it, over video that is supposed to reach the edge of the screen.
///
/// The border goes with the corners for the same reason: DWM strokes it in the
/// system colour, which is a pale hairline against a dark frame. It belongs on
/// a floating window and nowhere near one covering the display.
///
/// Pre-11 neither attribute exists and both calls fail, which is why the
/// results are discarded — Windows 10 rounds nothing, so square is right there.
fn apply_corner_style(hwnd: HWND) {
    let floating = WIN_STATE.load(Ordering::Relaxed) == NORMAL;
    let pref: DWM_WINDOW_CORNER_PREFERENCE = if floating { DWMWCP_ROUND } else { DWMWCP_DONOTROUND };
    let border: u32 = if floating { DWMWA_COLOR_DEFAULT } else { DWMWA_COLOR_NONE };
    // SAFETY: `hwnd` valid; each attribute pointer is a live value of the size
    // the matching attribute expects.
    unsafe {
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, std::ptr::addr_of!(pref).cast(), core::mem::size_of_val(&pref) as u32);
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_BORDER_COLOR, std::ptr::addr_of!(border).cast(), core::mem::size_of_val(&border) as u32);
    }
}

/// Hook the single Slint window: store its HWND, accept file drops, install the
/// icon, and subclass it so WM_DROPFILES / resize are handled.
pub fn attach_ui(ui: &slint::Window) -> bool {
    let Some(hwnd) = hwnd_of(ui) else { return false };
    ensure_app_icon(hwnd);
    enforce_frameless(hwnd);
    disable_dwm_transitions(hwnd);
    apply_corner_style(hwnd);
    // SAFETY: documented subclassing technique; we chain to the original proc.
    unsafe {
        let orig = SetWindowLongPtrW(hwnd, GWLP_WNDPROC, ui_subclass as WndProcFn as usize as isize);
        ORIG_WNDPROC.store(orig, Ordering::Relaxed);
        DragAcceptFiles(hwnd, true);
    }
    true
}

unsafe extern "system" fn ui_subclass(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    // Treat the whole window as client area — no native title bar / border —
    // regardless of style. winit's frameless handling is flaky under the
    // femtovg backend (a classic caption sometimes appears); this kills it for
    // the window's whole lifetime. WS_THICKFRAME is kept so resizing still works.
    // Canonical custom-chrome suppression. winit's femtovg backend re-decorates
    // the (transparent) window on activation, so a native caption flashes in.
    // These three together keep the whole window client-area and stop Windows
    // from ever painting a non-client frame:
    //   • NCCALCSIZE → 0: the client area covers the entire window.
    //   • NCPAINT → 0: never paint the non-client frame.
    //   • NCACTIVATE → DefWindowProc with lParam -1: skip the caption repaint on
    //     focus changes while still reporting activation.
    if msg == WM_NCCALCSIZE && w.0 != 0 {
        return LRESULT(0);
    }
    if msg == WM_NCPAINT {
        return LRESULT(0);
    }
    if msg == WM_NCACTIVATE {
        // Chain to winit (so it still tracks focus/activation, which drives input
        // and redraws) but force lParam = -1 so the native caption is never
        // repainted on focus changes.
        let orig = ORIG_WNDPROC.load(Ordering::Relaxed);
        // SAFETY: `orig` is winit's replaced proc; -1 suppresses the NC repaint.
        return unsafe {
            let f: WndProcFn = core::mem::transmute(orig);
            CallWindowProcW(Some(f), hwnd, msg, w, LPARAM(-1))
        };
    }
    if msg == WM_DROPFILES {
        // SAFETY: wparam is a valid HDROP owned by us until DragFinish.
        unsafe {
            let hdrop = HDROP(w.0 as *mut core::ffi::c_void);
            let len = DragQueryFileW(hdrop, 0, None) as usize;
            if len > 0 {
                let mut buf = vec![0u16; len + 1];
                DragQueryFileW(hdrop, 0, Some(&mut buf));
                let path = String::from_utf16_lossy(&buf[..len]);
                DragFinish(hdrop);
                DROP_HANDLER.with(|h| { if let Some(f) = h.borrow().as_ref() { f(path); } });
            } else {
                DragFinish(hdrop);
            }
        }
        return LRESULT(0);
    }
    let orig = ORIG_WNDPROC.load(Ordering::Relaxed);
    // SAFETY: `orig` is the replaced proc; chaining keeps winit/Slint working.
    unsafe {
        let f: WndProcFn = core::mem::transmute(orig);
        CallWindowProcW(Some(f), hwnd, msg, w, l)
    }
}

fn monitor_rect(hwnd: HWND, work_area: bool) -> Option<RECT> {
    // SAFETY: `hwnd` valid; `mi` is a live MONITORINFO with cbSize set.
    unsafe {
        let mut mi = MONITORINFO { cbSize: core::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        GetMonitorInfoW(MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST), &raw mut mi)
            .as_bool()
            .then_some(if work_area { mi.rcWork } else { mi.rcMonitor })
    }
}

fn set_rect(hwnd: HWND, r: RECT, topmost: bool) {
    let after = if topmost { HWND_TOPMOST } else { HWND_NOTOPMOST };
    // SAFETY: `hwnd` valid; SetWindowPos moves/resizes; winit picks up WM_SIZE.
    unsafe {
        let _ = SetWindowPos(hwnd, Some(after), r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOACTIVATE);
    }
}

fn current_rect(hwnd: HWND) -> Option<RECT> {
    let mut r = RECT::default();
    // SAFETY: `hwnd` valid; GetWindowRect only reads.
    unsafe { GetWindowRect(hwnd, &raw mut r).ok()?; }
    Some(r)
}

/// Toggle borderless fullscreen (covers the whole monitor). Returns the new state.
pub fn toggle_fullscreen(win: &slint::Window) -> bool {
    let Some(ui) = hwnd_of(win) else { return false };
    if WIN_STATE.load(Ordering::Relaxed) == FULLSCREEN {
        WIN_STATE.store(NORMAL, Ordering::Relaxed);
        apply_corner_style(ui);
        if let Some(r) = SAVED_RECT.lock().ok().and_then(|mut g| g.take()) {
            set_rect(ui, r, ALWAYS_ON_TOP.load(Ordering::Relaxed));
        }
        false
    } else {
        if WIN_STATE.load(Ordering::Relaxed) == NORMAL {
            if let (Some(r), Ok(mut g)) = (current_rect(ui), SAVED_RECT.lock()) {
                *g = Some(r);
            }
        }
        WIN_STATE.store(FULLSCREEN, Ordering::Relaxed);
        apply_corner_style(ui);
        if let Some(r) = monitor_rect(ui, false) {
            set_rect(ui, r, true);
        }
        true
    }
}

/// Toggle maximize to the monitor work area. No-op while fullscreen.
pub fn toggle_maximize(win: &slint::Window) {
    let Some(ui) = hwnd_of(win) else { return };
    if WIN_STATE.load(Ordering::Relaxed) == FULLSCREEN {
        return;
    }
    if WIN_STATE.load(Ordering::Relaxed) == MAXIMIZED {
        WIN_STATE.store(NORMAL, Ordering::Relaxed);
        apply_corner_style(ui);
        if let Some(r) = SAVED_RECT.lock().ok().and_then(|mut g| g.take()) {
            set_rect(ui, r, ALWAYS_ON_TOP.load(Ordering::Relaxed));
        }
    } else {
        if let (Some(r), Ok(mut g)) = (current_rect(ui), SAVED_RECT.lock()) {
            *g = Some(r);
        }
        WIN_STATE.store(MAXIMIZED, Ordering::Relaxed);
        apply_corner_style(ui);
        if let Some(r) = monitor_rect(ui, true) {
            set_rect(ui, r, ALWAYS_ON_TOP.load(Ordering::Relaxed));
        }
    }
}

/// Pin the window above the others. Always honoured here — the return exists
/// for the Wayland case on the other side of the parent module's contract.
pub fn set_always_on_top(win: &slint::Window, enabled: bool) -> bool {
    ALWAYS_ON_TOP.store(enabled, Ordering::Relaxed);
    let Some(ui) = hwnd_of(win) else { return false };
    let after = if enabled { HWND_TOPMOST } else { HWND_NOTOPMOST };
    // SAFETY: `ui` valid; SWP only reorders.
    unsafe {
        let _ = SetWindowPos(ui, Some(after), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
    }
    true
}

/// Begin a window move via the title bar. The frame is stripped (no native
/// caption to grab), so we hand the modal move loop to Windows directly:
/// release the implicit mouse capture, then post a caption-hit non-client
/// button-down. Works even with WS_CAPTION cleared.
pub fn start_drag(win: &slint::Window) {
    let Some(ui) = hwnd_of(win) else { return };
    if WIN_STATE.load(Ordering::Relaxed) != NORMAL {
        return;
    }
    // SAFETY: `ui` is valid; both calls are standard for frameless drag.
    unsafe {
        let _ = ReleaseCapture();
        SendMessageW(ui, WM_NCLBUTTONDOWN, Some(WPARAM(HTCAPTION as usize)), Some(LPARAM(0)));
        // SendMessage only returns once Windows' modal move loop ends — and that
        // loop swallowed the mouse-up, so winit/Slint still believes the left
        // button is held and ignores every later click (the UI looks frozen).
        // Synthesize the release at the cursor to clear the stuck input state.
        let mut pt = POINT::default();
        if GetCursorPos(&raw mut pt).is_ok() {
            let _ = ScreenToClient(ui, &raw mut pt);
            let lp = LPARAM(((pt.y << 16) | (pt.x & 0xffff)) as isize);
            let _ = PostMessageW(Some(ui), WM_LBUTTONUP, WPARAM(0), lp);
        }
    }
}

pub fn minimize(win: &slint::Window) {
    let Some(ui) = hwnd_of(win) else { return };
    // SAFETY: `ui` valid; ShowWindow only changes show-state.
    unsafe {
        let _ = ShowWindow(ui, SW_MINIMIZE);
    }
}

/// Show or hide the OS mouse cursor (used by the fullscreen idle auto-hide).
/// `ShowCursor` keeps an internal display counter, so the swap-guard ensures we
/// only ever decrement once to hide and increment once to show — never letting
/// the counter drift out of balance. Unlike Slint's reactive `mouse-cursor`,
/// this takes effect immediately even while the pointer is stationary.
pub fn set_cursor_hidden(_win: &slint::Window, hidden: bool) {
    if CURSOR_HIDDEN.swap(hidden, Ordering::Relaxed) == hidden {
        return;
    }
    // SAFETY: ShowCursor only adjusts the system cursor display counter.
    unsafe {
        let _ = ShowCursor(!hidden);
    }
}

