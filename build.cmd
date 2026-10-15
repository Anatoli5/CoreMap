@echo off
rem Builds go through tools\check.ps1 into build\ and reach the project root only through tools\deliver.ps1
rem (DECISIONS #33). This file used to write CoreMap.exe directly over the owner's running program.
echo Use: powershell -NoProfile -ExecutionPolicy Bypass -File tools\check.ps1 -BuildOnly
exit /b 1
