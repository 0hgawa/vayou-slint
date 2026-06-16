fn main() {
    // Bundle the gettext .po translations (lang/<code>/LC_MESSAGES/vayou.po) into
    // the binary so the UI switches language at runtime, keyed by the plain
    // English string — no per-component translation context.
    let cfg = slint_build::CompilerConfiguration::new()
        .with_bundled_translations("lang")
        .with_default_translation_context(slint_build::DefaultTranslationContext::None);
    slint_build::compile_with_config("ui/app.slint", cfg).expect("compile app.slint");
    println!("cargo:rerun-if-changed=lang");

    // Embed the brand icon as the Windows exe/taskbar icon — compile-time only.
    embed_resource::compile("app.rc", embed_resource::NONE)
        .manifest_required()
        .expect("embed app icon");
    println!("cargo:rerun-if-changed=app.rc");
    println!("cargo:rerun-if-changed=assets/icon.ico");

    // Stamp the exe (debug AND release) with Windows version metadata, generated
    // from the crate version so the file's properties (File/Product version) are
    // always in sync with Cargo.toml — one source of truth, no hand-edited .rc.
    embed_version_resource();
}

/// Write a `VERSIONINFO` resource derived from `CARGO_PKG_VERSION` to `OUT_DIR`
/// and compile it into the binary, so the exe reports its version in Explorer.
fn embed_version_resource() {
    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR");
    let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".into());
    let major = std::env::var("CARGO_PKG_VERSION_MAJOR").ok().and_then(|v| v.parse().ok()).unwrap_or(0u16);
    let minor = std::env::var("CARGO_PKG_VERSION_MINOR").ok().and_then(|v| v.parse().ok()).unwrap_or(0u16);
    let patch = std::env::var("CARGO_PKG_VERSION_PATCH").ok().and_then(|v| v.parse().ok()).unwrap_or(0u16);

    let rc = format!(
        "1 VERSIONINFO\n\
         FILEVERSION {major},{minor},{patch},0\n\
         PRODUCTVERSION {major},{minor},{patch},0\n\
         FILEFLAGSMASK 0x3fL\n\
         FILEFLAGS 0x0L\n\
         FILEOS 0x40004L\n\
         FILETYPE 0x1L\n\
         FILESUBTYPE 0x0L\n\
         BEGIN\n\
         \x20 BLOCK \"StringFileInfo\"\n\
         \x20 BEGIN\n\
         \x20   BLOCK \"040904b0\"\n\
         \x20   BEGIN\n\
         \x20     VALUE \"CompanyName\", \"Ohgawa\"\n\
         \x20     VALUE \"FileDescription\", \"Vayou - native Windows video player\"\n\
         \x20     VALUE \"FileVersion\", \"{version}\"\n\
         \x20     VALUE \"InternalName\", \"vayou\"\n\
         \x20     VALUE \"OriginalFilename\", \"vayou.exe\"\n\
         \x20     VALUE \"ProductName\", \"Vayou\"\n\
         \x20     VALUE \"ProductVersion\", \"{version}\"\n\
         \x20     VALUE \"LegalCopyright\", \"Copyright (C) 2026 Ohgawa\"\n\
         \x20   END\n\
         \x20 END\n\
         \x20 BLOCK \"VarFileInfo\"\n\
         \x20 BEGIN\n\
         \x20   VALUE \"Translation\", 0x409, 1200\n\
         \x20 END\n\
         END\n"
    );

    let rc_path = std::path::Path::new(&out_dir).join("version.rc");
    std::fs::write(&rc_path, rc).expect("write version.rc");
    embed_resource::compile(&rc_path, embed_resource::NONE)
        .manifest_optional()
        .expect("embed version info");
}
