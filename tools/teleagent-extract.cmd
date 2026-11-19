@echo off
setlocal
chcp 65001 >nul
where node >nul 2>nul
if errorlevel 1 (
  echo [!] Node.js not found. Install Node 18+ first.
  pause
  exit /b 1
)
node "%~dp0teleagent-extract.js" %*
if errorlevel 1 pause
exit /b %errorlevel%