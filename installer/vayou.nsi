; Vayou — per-user installer (no admin, no UAC).
;
; Build it with NSIS (https://nsis.sourceforge.io):
;   makensis vayou.nsi
; (or just run build.ps1 next to this file, which also signs the binary and
; emits latest.json for the in-app self-updater.)
;
; Lives in installer/ inside the repo. The .nsi and build.ps1 are tracked; the
; built Vayou-Setup.exe / vayou.exe.minisig / latest.json are git-ignored.
;
; No runtime bootstrapper: the Slint app is a single native exe. The payload is
; vayou.exe + the two sidecars it loads at runtime — libmpv-2.dll (video engine)
; and ffmpeg.exe (subtitle extraction / translation). Translations, icons and
; the per-format file-type icons are compiled into the binary; the app registers
; its file associations itself on first run (src/file_assoc.rs), so the
; installer doesn't touch them — only the uninstaller cleans them up.

Unicode true
; Without this the installer is bitmap-stretched on high-DPI screens (blurry
; text); the manifest makes Windows render it crisply at the real DPI.
ManifestDPIAware true

!define APP_NAME    "Vayou"
!define APP_EXE     "vayou.exe"
; Version is normally passed by build.ps1 (/DAPP_VERSION=… /DAPP_VERSION_4=…),
; read from Cargo.toml so it stays the single source of truth. These defaults
; apply only when makensis is run directly.
!ifndef APP_VERSION
  !define APP_VERSION "0.1.0"
!endif
!ifndef APP_VERSION_4
  !define APP_VERSION_4 "0.1.0.0"
!endif
!define PUBLISHER   "Ohgawa"
!define APP_ID      "Vayou"
!define UNINST_KEY  "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APP_ID}"

; Source artefacts (the built release). Absolute paths so makensis can run from
; anywhere; spaces are fine inside the quotes.
!define SRC    "D:\Apps\vayou-slint\target\release"
!define ASSETS "D:\Apps\vayou-slint\assets"
; The installer's build output lands in this folder (installer/).
!define HERE   "D:\Apps\vayou-slint\installer"

Name "${APP_NAME}"
; Absolute so the output always lands in installer/ regardless of makensis' cwd.
OutFile "${HERE}\Vayou-Setup.exe"
RequestExecutionLevel user
InstallDir "$LOCALAPPDATA\Programs\${APP_NAME}"
InstallDirRegKey HKCU "Software\${APP_ID}" "InstallDir"
SetCompressor /SOLID lzma
ShowInstDetails show
ShowUninstDetails show
BrandingText "Copyright (c) 2026 ${PUBLISHER}"

!include "MUI2.nsh"
!include "FileFunc.nsh"

!define MUI_ICON   "${ASSETS}\icon.ico"
!define MUI_UNICON "${ASSETS}\icon.ico"
!define MUI_FINISHPAGE_RUN "$INSTDIR\${APP_EXE}"
!define MUI_FINISHPAGE_RUN_TEXT "Launch Vayou"

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "English"

VIProductVersion "${APP_VERSION_4}"
VIAddVersionKey "ProductName"     "${APP_NAME}"
VIAddVersionKey "FileVersion"     "${APP_VERSION}"
VIAddVersionKey "ProductVersion"  "${APP_VERSION}"
VIAddVersionKey "CompanyName"     "${PUBLISHER}"
VIAddVersionKey "LegalCopyright"  "Copyright (c) 2026 ${PUBLISHER}"
VIAddVersionKey "FileDescription" "${APP_NAME} Setup"

; Close a running instance so its files (and the locked libmpv-2.dll) are free.
!macro CloseRunning
  nsExec::Exec 'taskkill /IM ${APP_EXE} /F /T'
!macroend

; Remove the per-extension association the app registers at runtime (HKCU only):
; the Vayou.<ext> ProgID, the "Open with" entry, and the default handler if it
; still points at us.
!macro UnregExt EXT
  DeleteRegKey   HKCU "Software\Classes\Vayou.${EXT}"
  DeleteRegValue HKCU "Software\Classes\.${EXT}\OpenWithProgids" "Vayou.${EXT}"
  DeleteRegValue HKCU "Software\Classes\.${EXT}" ""
!macroend

Function .onInit
  ; Re-running over an existing install just updates in place: InstallDirRegKey
  ; resolves $INSTDIR to the current location and the files overwrite (the app's
  ; config in %APPDATA%\Vayou is kept). Uninstall lives in Windows' Add/Remove
  ; Programs, where it has a real button.
  !insertmacro CloseRunning
FunctionEnd

Section "Install"
  SetOutPath "$INSTDIR"
  File "${SRC}\${APP_EXE}"
  File "${SRC}\libmpv-2.dll"
  File "${SRC}\ffmpeg.exe"

  CreateShortcut "$SMPROGRAMS\${APP_NAME}.lnk" "$INSTDIR\${APP_EXE}"
  CreateShortcut "$DESKTOP\${APP_NAME}.lnk"    "$INSTDIR\${APP_EXE}"

  WriteUninstaller "$INSTDIR\Uninstall.exe"
  WriteRegStr HKCU "Software\${APP_ID}" "InstallDir" "$INSTDIR"

  ; Add/Remove Programs (per-user).
  WriteRegStr   HKCU "${UNINST_KEY}" "DisplayName"     "${APP_NAME}"
  WriteRegStr   HKCU "${UNINST_KEY}" "DisplayVersion"  "${APP_VERSION}"
  WriteRegStr   HKCU "${UNINST_KEY}" "Publisher"       "${PUBLISHER}"
  WriteRegStr   HKCU "${UNINST_KEY}" "DisplayIcon"     "$INSTDIR\${APP_EXE}"
  WriteRegStr   HKCU "${UNINST_KEY}" "UninstallString" '"$INSTDIR\Uninstall.exe"'
  WriteRegStr   HKCU "${UNINST_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegDWORD HKCU "${UNINST_KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNINST_KEY}" "NoRepair" 1
  ${GetSize} "$INSTDIR" "/S=0K" $0 $1 $2
  IntFmt $0 "0x%08X" $0
  WriteRegDWORD HKCU "${UNINST_KEY}" "EstimatedSize" "$0"

  ; On a silent install (Setup.exe /S) the Finish page's "run" checkbox never
  ; shows, so launch the app ourselves. (The in-app updater swaps vayou.exe
  ; directly, so this only covers a manual /S.)
  IfSilent 0 +2
    Exec '"$INSTDIR\${APP_EXE}"'
SectionEnd

Section "Uninstall"
  !insertmacro CloseRunning

  ; Program files.
  Delete "$INSTDIR\${APP_EXE}"
  Delete "$INSTDIR\libmpv-2.dll"
  Delete "$INSTDIR\ffmpeg.exe"
  Delete "$INSTDIR\Uninstall.exe"
  RMDir  "$INSTDIR"

  ; Shortcuts.
  Delete "$SMPROGRAMS\${APP_NAME}.lnk"
  Delete "$DESKTOP\${APP_NAME}.lnk"

  ; Undo the file associations the app created at runtime (src/file_assoc.rs).
  !insertmacro UnregExt "mp4"
  !insertmacro UnregExt "mkv"
  !insertmacro UnregExt "avi"
  !insertmacro UnregExt "mov"
  !insertmacro UnregExt "webm"
  !insertmacro UnregExt "flv"
  !insertmacro UnregExt "wmv"
  !insertmacro UnregExt "m4v"
  !insertmacro UnregExt "ts"
  !insertmacro UnregExt "mpg"
  !insertmacro UnregExt "mpeg"
  !insertmacro UnregExt "mp3"
  !insertmacro UnregExt "flac"
  !insertmacro UnregExt "ogg"
  !insertmacro UnregExt "wav"
  !insertmacro UnregExt "aac"
  !insertmacro UnregExt "wma"
  !insertmacro UnregExt "m4a"
  !insertmacro UnregExt "opus"
  ; The generated file-type icon cache (config.json next to it is user data — kept).
  RMDir /r "$LOCALAPPDATA\Vayou\fileicons"

  ; Tell the shell associations changed so Explorer refreshes icons / "Open with".
  System::Call 'shell32::SHChangeNotify(i 0x08000000, i 0, i 0, i 0)'

  ; Our own keys.
  DeleteRegKey HKCU "${UNINST_KEY}"
  DeleteRegKey HKCU "Software\${APP_ID}"
SectionEnd
