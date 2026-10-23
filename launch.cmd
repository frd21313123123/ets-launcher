@echo off
cd /d "%~dp0"
if exist "dist\ets-launcher.exe" (
    start "" "dist\ets-launcher.exe"
    exit /b
)
if exist "target\release\ets-launcher.exe" (
    start "" "target\release\ets-launcher.exe"
    exit /b
)
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\cargo.ps1 run --release
if errorlevel 1 pause
