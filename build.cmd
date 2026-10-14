@echo off
rem Builds CoreMap.exe with the C# compiler that ships with Windows (.NET Framework 4.x). Nothing to install.
setlocal
cd /d "%~dp0"
rem The new window's page (page\) is built first: TypeScript compiler from node_modules\, see tools\page.ps1.
powershell -NoProfile -ExecutionPolicy Bypass -File tools\page.ps1 || exit /b 2
set CSC=%WINDIR%\Microsoft.NET\Framework64\v4.0.30319\csc.exe
set ICON=
if exist assets\coremap.ico set ICON=/win32icon:assets\coremap.ico
"%CSC%" /nologo /codepage:65001 /target:winexe /optimize+ /platform:x64 /win32manifest:app.manifest %ICON% ^
  /r:System.Windows.Forms.dll /r:System.Drawing.dll /r:System.Management.dll ^
  /r:webview2\Microsoft.Web.WebView2.Core.dll /r:webview2\Microsoft.Web.WebView2.WinForms.dll /out:CoreMap.exe src\*.cs
exit /b %ERRORLEVEL%
