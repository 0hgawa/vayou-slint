@echo off
rem Build and run the debug build, teeing everything to dev.log.
rem
rem Mirrors the Tauri build's dev.bat, with two differences this project needs:
rem the log level comes from VAYOU_LOG (not RUST_LOG), and libmpv has to be
rem reachable — the app looks next to the exe, which in a dev build is
rem target\debug\, while the DLL lives in binaries\.

cd /d "%~dp0"
set LOG=dev.log

rem Verbose by default: the point of this script is diagnosing a running app,
rem and at the default (warn) the log shows almost nothing. Values: trace,
rem debug, info, error. Override with:  set VAYOU_LOG=info  &&  dev.bat
if not defined VAYOU_LOG set VAYOU_LOG=debug

rem libmpv-2.dll and ffmpeg.exe are not next to the debug exe. The loader falls
rem back to the system search path, so putting binaries\ on PATH is enough and
rem avoids copying 220 MB into target\ on every build.
set PATH=%CD%\binaries;%PATH%

if not exist "binaries\libmpv-2.dll" (
  echo.
  echo   binaries\libmpv-2.dll is missing — the app will start and fail to
  echo   load mpv. Fetch it as the README describes, then run this again.
  echo.
)

echo === Vayou (Slint) dev ===
echo Log file:   %CD%\%LOG%
echo VAYOU_LOG:  %VAYOU_LOG%
echo.

powershell -NoProfile -ExecutionPolicy Bypass -Command "& { cargo run 2>&1 | Tee-Object -FilePath '%LOG%'; exit $LASTEXITCODE }"
set ERR=%ERRORLEVEL%

echo.
echo ===================================
echo Exit code: %ERR%
echo Full log:  %CD%\%LOG%
echo ===================================
pause
