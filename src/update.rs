//! Signed self-update. The app checks a small JSON feed published with each
//! GitHub release, and — when a newer build exists — downloads just the new
//! binary, verifies its minisign signature against the embedded public key, and
//! swaps it in place (the per-user install dir is writable without elevation).
//! A tampered or unsigned download is rejected before it ever replaces the
//! running binary.
//!
//! Only the app binary is fetched. On Windows the bundled `libmpv-2.dll` /
//! `ffmpeg.exe` from the installer are left untouched, and a libmpv bump ships a
//! fresh installer instead; on Unix both are distribution packages this has no
//! business touching.

use std::sync::OnceLock;
use std::time::Duration;

use crate::error::LogErr;

/// GitHub `owner/repo` that publishes Vayou releases.
const REPO: &str = "0hgawa/vayou-slint";

/// The key this build looks up in the feed's `platforms` map. A build only ever
/// updates itself from its own platform's entry, so a feed carrying just one
/// platform reads as "no update" for the others rather than as a broken feed.
const PLATFORM_KEY: &str = if cfg!(windows) { "windows-x86_64" } else { "linux-x86_64" };

/// Name for the downloaded binary while it is staged in the temp directory.
/// Windows refuses to execute an extension-less file, and `self_replace` runs
/// it; Unix has no such rule and an `.exe` there would just be a lie.
#[cfg(windows)]
const STAGED_NAME: &str = "vayou-update.exe";
#[cfg(unix)]
const STAGED_NAME: &str = "vayou-update";

/// The release feed: a small JSON manifest uploaded with each GitHub release.
/// Schema: `{ version, platforms: { "windows-x86_64": { url, signature },
/// "linux-x86_64": { … } } }`, where `signature` is the raw minisign `.minisig`
/// text for that platform's binary.
const UPDATE_FEED: &str = "https://github.com/0hgawa/vayou-slint/releases/latest/download/latest.json";

/// minisign public key the downloaded binary must be signed with. The matching
/// secret key (`.keys/vayou.key`) signs it in CI, from the `MINISIGN_SECRET_KEY`
/// secret on the `release` environment — `installer/build.ps1` and
/// `installer/build-linux.sh` no longer sign by hand.
///
/// Unchanged since 0.1.0 on purpose: every installed build verifies against this
/// value, so rotating it strands all of them on a manual reinstall. The key is
/// rsign-format with an empty passphrase, which is what lets CI sign
/// non-interactively without a password secret.
const UPDATE_PUBKEY: &str = "RWQqB0vT3F3JFedzm8aLV556vRHx3wvHamu34WDK+MWI09SXuo9LDEze";

/// A newer release: version, the new binary's URL, and its minisign signature —
/// all that's needed to download, verify and swap it in.
#[derive(Clone)]
pub struct UpdateInfo {
    pub version: String,
    url: String,
    signature: String,
}

fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent(concat!("Vayou/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())
}

/// `(major, minor, patch)` from a `v1.2.3`-ish tag, ignoring pre-release/build.
fn semver(v: &str) -> (u32, u32, u32) {
    let mut it = v.trim().trim_start_matches('v').split(['.', '-', '+']);
    let mut next = || it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    (next(), next(), next())
}

/// Check the release feed; `Some(UpdateInfo)` when a newer build is published.
/// Async — drive it on a worker runtime.
pub async fn check() -> Result<Option<UpdateInfo>, String> {
    let resp = client()?.get(UPDATE_FEED).send().await.map_err(|e| e.to_string())?;
    // No release (or no feed asset) published yet → nothing newer than us.
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status().as_u16()));
    }
    let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    let version = json.get("version").and_then(|v| v.as_str()).ok_or("no version in feed")?;
    if semver(version) <= semver(env!("CARGO_PKG_VERSION")) {
        return Ok(None);
    }
    let plat = json
        .pointer(&format!("/platforms/{PLATFORM_KEY}"))
        .ok_or_else(|| format!("no {PLATFORM_KEY} in feed"))?;
    let url = plat.get("url").and_then(|v| v.as_str()).ok_or("no url in feed")?;
    let signature = plat.get("signature").and_then(|v| v.as_str()).ok_or("no signature in feed")?;
    Ok(Some(UpdateInfo {
        version: version.to_owned(),
        url: url.to_owned(),
        signature: signature.to_owned(),
    }))
}

/// Download the new binary, verify its minisign signature against the embedded
/// public key, and swap it in for the running executable in place. The caller
/// then relaunches and exits so the new image takes over. Async — drive it on a
/// worker runtime.
pub async fn download_and_apply(info: &UpdateInfo) -> Result<(), String> {
    let bytes = client()?
        .get(&info.url)
        .send().await.map_err(|e| e.to_string())?
        .error_for_status().map_err(|e| e.to_string())?
        .bytes().await.map_err(|e| e.to_string())?;
    verify_signature(&bytes, &info.signature)?;
    let staged = std::env::temp_dir().join(STAGED_NAME);
    std::fs::write(&staged, &bytes).map_err(|e| format!("write update: {e}"))?;
    let swapped = self_replace::self_replace(&staged).map_err(|e| describe_replace_error(&e));
    // Remove the staging copy either way — a failed swap should not leave a
    // verified binary sitting in the temp directory.
    let _ = std::fs::remove_file(&staged);
    swapped
}

/// How this build is installed, as the token the About page switches on:
/// `"self"`, `"flatpak"` or `"managed"`.
///
/// Offering an update that cannot be applied is worse than offering none: the
/// user clicks, waits out a download, and is handed an error about their own
/// filesystem that they can do nothing with. `describe_replace_error` still
/// explains a swap that fails for some other reason — this is what keeps the
/// button from being drawn when the swap could never have worked at all.
///
/// Answered once and remembered: a running binary does not move, and the probe
/// below touches the filesystem, which is not worth repeating every time the
/// panel opens.
pub fn install_kind() -> &'static str {
    static KIND: OnceLock<&'static str> = OnceLock::new();
    KIND.get_or_init(|| {
        // A Flatpak sandbox mounts its own metadata at the root. The app lives
        // in a read-only `/app` there and updating it belongs to the runtime —
        // the user runs `flatpak update`, or their software centre does it.
        if std::path::Path::new("/.flatpak-info").exists() {
            return "flatpak";
        }
        // Everything else reduces to one question: can this process create a
        // file in the directory holding the binary? That is precisely what
        // `self_replace` needs, since it stages the replacement there before
        // renaming it over. Asking the filesystem beats matching path prefixes,
        // which would misjudge both a user-owned /opt install and a root-owned
        // ~/.local/bin.
        std::env::current_exe().map_or("managed", |exe| {
            if exe_dir_accepts_new_files(&exe) { "self" } else { "managed" }
        })
    })
}

/// Whether a new file can be created beside `exe`.
fn exe_dir_accepts_new_files(exe: &std::path::Path) -> bool {
    let Some(dir) = exe.parent() else { return false };
    // The pid keeps two instances probing at the same moment off one name.
    let probe = dir.join(format!(".vayou-write-probe-{}", std::process::id()));
    std::fs::File::create(&probe).is_ok_and(|_| {
        std::fs::remove_file(&probe).log_err("remove update write probe");
        true
    })
}

/// Turn a failed in-place swap into something the user can act on.
///
/// The Windows installer is per-user and its directory is writable, so a denial
/// there is unusual. On Unix the binary is as likely to sit under `/usr`, owned
/// by root and managed by a package — where no retry will ever succeed and the
/// honest answer is to say so and point at the release page.
fn describe_replace_error(e: &std::io::Error) -> String {
    if e.kind() == std::io::ErrorKind::PermissionDenied {
        return "no permission to replace the running binary — it is installed system-wide, so update it through your package manager or download the new release manually".to_owned();
    }
    format!("replace executable: {e}")
}

/// Reject a download whose minisign signature doesn't match the embedded key.
fn verify_signature(bytes: &[u8], signature: &str) -> Result<(), String> {
    use minisign_verify::{PublicKey, Signature};
    let pk = PublicKey::from_base64(UPDATE_PUBKEY).map_err(|e| format!("public key: {e}"))?;
    let sig = Signature::decode(signature).map_err(|e| format!("signature: {e}"))?;
    pk.verify(bytes, &sig, false)
        .map_err(|_| "signature does not match — refusing to install the download".to_owned())
}

/// Relaunch the (just-replaced) executable and let the caller quit the current
/// one, so the new image takes over. Vayou isn't single-instance, so a plain
/// fresh launch is all that's needed.
pub fn relaunch() {
    let Ok(exe) = std::env::current_exe() else { return };
    let mut child = std::process::Command::new(exe);
    // Detach, so the new image is not tied to this process's console or job. A
    // Unix child already outlives its parent, so there is nothing to add there.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        child.creation_flags(DETACHED_PROCESS);
    }
    let _ = child.spawn();
}

/// Open the releases page in the default browser (no console window). Offered as
/// a fallback when the in-app update can't be applied.
pub fn open_release_page() {
    let url = format!("https://github.com/{REPO}/releases");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let _ = std::process::Command::new("cmd")
            .args(["/c", "start", "", &url])
            .creation_flags(CREATE_NO_WINDOW)
            .spawn();
    }
    // The freedesktop way in: whatever the session set as the browser. Present
    // on every desktop that ships a portal, which the file dialog already needs.
    #[cfg(unix)]
    let _ = std::process::Command::new("xdg-open").arg(&url).spawn();
}

#[cfg(test)]
mod tests {
    use super::semver;

    #[test]
    fn parses_versions_ignoring_prefix_and_suffix() {
        assert_eq!(semver("v1.2.3"), (1, 2, 3));
        assert_eq!(semver("1.2.3"), (1, 2, 3));
        // Pre-release / build metadata is dropped at the first separator.
        assert_eq!(semver("v2.0.0-rc1"), (2, 0, 0));
        assert_eq!(semver("1.4.0+build7"), (1, 4, 0));
        // Missing components default to zero; garbage parses as 0.0.0.
        assert_eq!(semver("v2"), (2, 0, 0));
        assert_eq!(semver("garbage"), (0, 0, 0));
    }

    #[test]
    fn newer_versions_compare_greater() {
        assert!(semver("v1.0.1") > semver("v1.0.0"));
        assert!(semver("v1.2.0") > semver("v1.1.9"));
        assert!(semver("v2.0.0") > semver("v1.9.9"));
        assert!(semver("v1.0.0") <= semver("v1.0.0"));
    }
}
