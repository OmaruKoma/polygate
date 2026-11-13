@echo off
setlocal
chcp 65001 >nul
where node >nul 2>nul
if errorlevel 1 (
  echo [!] Node.js not found. Install Node 22.5+ first.
  pause
  exit /b 1
)
node "%~dp0doubao-extract.js" %*
if errorlevel 1 pause
exit /b %errorlevel%
