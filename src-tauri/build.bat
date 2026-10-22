@echo off
REM ============================================================
REM  CC-Switch Huawei CodeAgent Build Script
REM  Usage: src-tauri\build.bat
REM ============================================================

cd /d "%~dp0"
if errorlevel 1 exit /b 1

echo ============================================================
echo  CC-Switch Huawei CodeAgent Build Script
echo ============================================================
echo.

REM Step 1: Initialize MSVC x64 build environment
echo [1/5] Initializing MSVC x64 build environment...
set "VSWHERE=%ProgramFiles(x86)%\Microsoft Visual Studio\Installer\vswhere.exe"
set "VSPATH="
if exist "%VSWHERE%" (
    for /f "usebackq delims=" %%i in (`"%VSWHERE%" -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath`) do set "VSPATH=%%i"
)

if defined VSPATH (
    call "%VSPATH%\VC\Auxiliary\Build\vcvarsall.bat" x64
) else (
    REM Fallback: try the usual install locations (2019/2022, BuildTools/Community/Professional).
    for %%d in (
        "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools"
        "C:\Program Files (x86)\Microsoft Visual Studio\2022\Community"
        "C:\Program Files (x86)\Microsoft Visual Studio\2019\BuildTools"
        "C:\Program Files (x86)\Microsoft Visual Studio\2019\Community"
        "C:\Program Files\Microsoft Visual Studio\2022\BuildTools"
        "C:\Program Files\Microsoft Visual Studio\2022\Community"
    ) do (
        if not defined VSPATH if exist "%%~d\VC\Auxiliary\Build\vcvarsall.bat" (
            set "VSPATH=%%~d"
            call "%%~d\VC\Auxiliary\Build\vcvarsall.bat" x64
        )
    )
)

if not defined VSPATH (
    echo ERROR: Failed to locate Visual Studio C++ build tools
    echo Install Visual Studio Build Tools with the "Desktop development with C++" workload.
    pause
    exit /b 1
)
if %ERRORLEVEL% neq 0 (
    echo ERROR: Failed to initialize MSVC environment
    pause
    exit /b 1
)
echo OK: MSVC environment initialized: %VSPATH%
echo.

REM Step 2: Verify tools
echo [2/5] Verifying build tools...
where cl.exe >nul 2>&1
if %ERRORLEVEL% neq 0 (
    echo ERROR: cl.exe not found. MSVC environment not initialized.
    pause
    exit /b 1
)
echo OK: cl.exe found
where link.exe >nul 2>&1
if %ERRORLEVEL% neq 0 (
    echo ERROR: link.exe not found. MSVC environment not initialized.
    pause
    exit /b 1
)
echo OK: link.exe found
if not defined CARGO_HOME set "CARGO_HOME=%USERPROFILE%\.cargo"
if exist "%CARGO_HOME%\bin\cargo.exe" set "PATH=%CARGO_HOME%\bin;%PATH%"
where cargo.exe >nul 2>&1
if %ERRORLEVEL% neq 0 (
    echo ERROR: cargo.exe not found. Install Rust via rustup and add %%USERPROFILE%%\.cargo\bin to PATH.
    pause
    exit /b 1
)
echo OK: cargo.exe found
echo.

REM Step 3: Set mirror source (for China mainland)
echo [3/5] Setting up mirror sources...
set NO_PROXY=mirrors.ustc.edu.cn,*.huawei.com
set no_proxy=mirrors.ustc.edu.cn,*.huawei.com
set RUSTUP_DIST_SERVER=https://rust.inhuawei.com/rustup-static
echo OK: Mirror sources configured
echo.

REM Step 4: Build frontend and release binary
echo [4/5] Building frontend and release binary (this may take 10-30 minutes)...
echo.

where node.exe >nul 2>&1
if errorlevel 1 (
    echo ERROR: Node.js not found. Install Node.js 20.19+ or 22.12+.
    exit /b 1
)
where corepack.cmd >nul 2>&1
if errorlevel 1 (
    echo ERROR: Corepack not found. Install Corepack to use the pinned pnpm version.
    exit /b 1
)

pushd ".."
call corepack pnpm build:renderer
if errorlevel 1 (
    popd
    echo ERROR: Frontend build failed. Run corepack pnpm install first if dependencies are missing.
    exit /b 1
)
popd

REM Check if Rust toolchain file exists and adjust if needed
if exist ..\rust-toolchain.toml (
    echo Found rust-toolchain.toml, ensuring correct version...
    rustup show active-toolchain 2>nul
    if %ERRORLEVEL% neq 0 (
        echo WARNING: No active toolchain found. Attempting to install 1.95.0...
        rustup toolchain install 1.95.0-x86_64-pc-windows-msvc --profile minimal
    )
)

echo Starting cargo build --release...
cargo build --release
if %ERRORLEVEL% neq 0 (
    echo.
    echo ============================================================
    echo  BUILD FAILED with exit code %ERRORLEVEL%
    echo ============================================================
    echo.
    echo Common causes:
    echo   - C1056 error: Disable antivirus real-time protection
    echo   - Network error: Configure cargo mirror source
    echo   - link.exe not found: Run vcvarsall.bat first
    echo.
    pause
    exit /b 1
)

REM Step 5: Verify build result
echo.
echo [5/5] Verifying build result...
if exist target\release\cc-switch.exe (
    echo ============================================================
    echo  BUILD SUCCESSFUL!
    echo ============================================================
    echo.
    dir target\release\cc-switch.exe | findstr "cc-switch.exe"
    echo.
    echo Binary location: %CD%\target\release\cc-switch.exe
    echo.
    echo You can now run: target\release\cc-switch.exe
    echo.
    echo For installers use: pnpm tauri build
) else (
    echo WARNING: cc-switch.exe not found in target\release\
    echo Check build output above for errors.
)
echo.
pause
