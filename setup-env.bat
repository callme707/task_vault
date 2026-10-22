@echo off
setlocal
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\setup-env.ps1" %*
if %ERRORLEVEL% neq 0 (
    echo.
    echo [WARN] Environment check or setup exited with code %ERRORLEVEL%.
    pause
    exit /b %ERRORLEVEL%
)
echo.
pause
