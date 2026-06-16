//! Per-user file associations (HKCU, no admin): register Vayou as the handler for
//! the supported media extensions, each with its own colour-coded file-type icon
//! (like the Tauri build). Idempotent — re-registers only when the executable path
//! changed (first run, or after a move / update).
//!
//! The per-format icons are bundled in the binary and written to
//! `%LOCALAPPDATA%\Vayou\fileicons` on registration; each extension gets its own
//! `Vayou.<ext>` ProgID whose `DefaultIcon` points at the matching `.ico`. Setting
//! `HKCU\Software\Classes\.<ext>` to that ProgID is what makes Explorer show the
//! icon and open the file with Vayou (passed as a command-line argument, which
//! `main` already loads). This only writes HKCU and is fully reversible.

use std::path::PathBuf;

use windows::core::PCWSTR;
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegGetValueW, RegSetValueExW, HKEY, HKEY_CURRENT_USER,
    KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ, RRF_RT_REG_SZ,
};
use windows::Win32::UI::Shell::{SHChangeNotify, SHCNE_ASSOCCHANGED, SHCNF_IDLIST};

/// Extension → bundled file-type icon. One ProgID + icon is registered per entry.
const ICONS: &[(&str, &[u8])] = &[
    ("mp4", include_bytes!("../assets/fileicons/mp4.ico")),
    ("mkv", include_bytes!("../assets/fileicons/mkv.ico")),
    ("avi", include_bytes!("../assets/fileicons/avi.ico")),
    ("mov", include_bytes!("../assets/fileicons/mov.ico")),
    ("webm", include_bytes!("../assets/fileicons/webm.ico")),
    ("flv", include_bytes!("../assets/fileicons/flv.ico")),
    ("wmv", include_bytes!("../assets/fileicons/wmv.ico")),
    ("m4v", include_bytes!("../assets/fileicons/m4v.ico")),
    ("ts", include_bytes!("../assets/fileicons/ts.ico")),
    ("mpg", include_bytes!("../assets/fileicons/mpg.ico")),
    ("mpeg", include_bytes!("../assets/fileicons/mpeg.ico")),
    ("mp3", include_bytes!("../assets/fileicons/mp3.ico")),
    ("flac", include_bytes!("../assets/fileicons/flac.ico")),
    ("ogg", include_bytes!("../assets/fileicons/ogg.ico")),
    ("wav", include_bytes!("../assets/fileicons/wav.ico")),
    ("aac", include_bytes!("../assets/fileicons/aac.ico")),
    ("wma", include_bytes!("../assets/fileicons/wma.ico")),
    ("m4a", include_bytes!("../assets/fileicons/m4a.ico")),
    ("opus", include_bytes!("../assets/fileicons/opus.ico")),
];

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Read a string value (`name` = None for the key's default value) under HKCU.
fn read_string(subkey: &str, name: Option<&str>) -> Option<String> {
    let sub = wide(subkey);
    let nm = name.map(wide);
    let nmptr = nm.as_ref().map_or(PCWSTR::null(), |w| PCWSTR(w.as_ptr()));
    let mut buf = [0u16; 1024];
    let mut cb = (buf.len() * 2) as u32;
    // SAFETY: `buf`/`cb` are live; RegGetValueW writes at most `cb` bytes.
    let rc = unsafe {
        RegGetValueW(HKEY_CURRENT_USER, PCWSTR(sub.as_ptr()), nmptr, RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr().cast()), Some(&raw mut cb))
    };
    if rc != ERROR_SUCCESS {
        return None;
    }
    let len = (cb as usize / 2).saturating_sub(1); // drop the trailing NUL
    Some(String::from_utf16_lossy(&buf[..len]))
}

/// Write a string value under HKCU\`subkey`, creating the key chain.
fn write_string(subkey: &str, name: Option<&str>, value: &str) -> bool {
    let sub = wide(subkey);
    let mut hkey = HKEY::default();
    // SAFETY: standard key creation under HKCU; `hkey` receives the handle.
    let rc = unsafe {
        RegCreateKeyExW(HKEY_CURRENT_USER, PCWSTR(sub.as_ptr()), None, PCWSTR::null(), REG_OPTION_NON_VOLATILE, KEY_WRITE, None, &raw mut hkey, None)
    };
    if rc != ERROR_SUCCESS {
        return false;
    }
    let val = wide(value);
    let nm = name.map(wide);
    let nmptr = nm.as_ref().map_or(PCWSTR::null(), |w| PCWSTR(w.as_ptr()));
    // SAFETY: `val` is a live NUL-terminated UTF-16 buffer; the byte view spans it.
    let bytes = unsafe { std::slice::from_raw_parts(val.as_ptr().cast::<u8>(), val.len() * 2) };
    // SAFETY: `hkey` is valid until we close it below.
    let rc2 = unsafe { RegSetValueExW(hkey, nmptr, None, REG_SZ, Some(bytes)) };
    // SAFETY: closing the handle we opened.
    unsafe { let _ = RegCloseKey(hkey); }
    rc2 == ERROR_SUCCESS
}

/// Write the bundled per-format icons to `%LOCALAPPDATA%\Vayou\fileicons`,
/// returning that directory.
fn write_icons() -> Option<PathBuf> {
    let dir = dirs::data_local_dir()?.join("Vayou").join("fileicons");
    std::fs::create_dir_all(&dir).ok()?;
    for (ext, bytes) in ICONS {
        let _ = std::fs::write(dir.join(format!("{ext}.ico")), bytes);
    }
    Some(dir)
}

/// Register (or refresh) the associations, unless they already point here.
pub fn ensure_registered() {
    let Ok(exe) = std::env::current_exe() else { return };
    let exe = exe.to_string_lossy().replace('/', "\\");
    let command = format!("\"{exe}\" \"%1\"");

    // Always refresh the bundled icons (cheap, on a background thread) so an
    // updated icon set takes effect on the next launch.
    let Some(icondir) = write_icons() else { return };

    // Registry already points here → icons refreshed, nothing else to do.
    if read_string("Software\\Classes\\Vayou.mp4\\shell\\open\\command", None).as_deref() == Some(command.as_str()) {
        return;
    }

    for (ext, _) in ICONS {
        let progid = format!("Vayou.{ext}");
        let icon = icondir.join(format!("{ext}.ico")).to_string_lossy().replace('/', "\\");
        let base = format!("Software\\Classes\\{progid}");
        write_string(&base, None, &format!("{} Media", ext.to_uppercase()));
        write_string(&format!("{base}\\DefaultIcon"), None, &icon);
        if !write_string(&format!("{base}\\shell\\open\\command"), None, &command) {
            return;
        }
        // Point the extension at our ProgID so Explorer shows the icon and opens
        // the file with Vayou; also list us under "Open with".
        write_string(&format!("Software\\Classes\\.{ext}"), None, &progid);
        write_string(&format!("Software\\Classes\\.{ext}\\OpenWithProgids"), Some(&progid), "");
    }

    // Tell the shell associations changed so icons / "Open with" refresh now.
    // SAFETY: the documented null-item form just broadcasts the change.
    unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None); }
}
