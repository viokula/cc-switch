@echo off
setlocal
cd /d "%~dp0.."
call "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvarsall.bat" x64
if errorlevel 1 exit /b 1
set "CFLAGS=/nologo /MD /Z7"
set "AWS_LC_SYS_CFLAGS=/nologo /MD /Z7 /W3 /we4013"
set "CRATE_CC_NO_DEFAULTS=1"
set "CARGO_HTTP_CHECK_REVOKE=false"
if not defined CARGO_BUILD_JOBS set "CARGO_BUILD_JOBS=1"
cargo clippy --manifest-path src-tauri/Cargo.toml -- -D warnings
endlocal & exit /b %ERRORLEVEL%
