# Project check for the agent: builds the sources into CoreMapNew.exe (the owner's CoreMap.exe is not touched)
# and takes hidden simulated window snapshots into outputs\<Name>\. Nothing appears on screen, no elevation prompt,
# no real load. Exit code 0 = built and every snapshot was written.
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File tools\check.ps1 -Name stage1
#
# Compare the pictures with outputs\base\ (taken from the build before the restructuring).
param([string]$Name = 'check', [switch]$BuildOnly)

$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

# A string literal broken by an automatic replacement leaves a line that is only: " +
$broken = Select-String -Path src\*.cs -Pattern '^\s*" \+\s*$'
if ($broken) { $broken | ForEach-Object { Write-Host ("broken literal: " + $_.Path + ":" + $_.LineNumber) }; exit 3 }

$csc = Join-Path $env:WINDIR 'Microsoft.NET\Framework64\v4.0.30319\csc.exe'
$out = Join-Path $root 'CoreMapNew.exe'
$icon = @()
if (Test-Path assets\coremap.ico) { $icon = @('/win32icon:assets\coremap.ico') }
& $csc /nologo /codepage:65001 /target:winexe /optimize+ /platform:x64 /win32manifest:app.manifest @icon `
    /r:System.Windows.Forms.dll /r:System.Drawing.dll /r:System.Web.Extensions.dll "/out:$out" src\*.cs
if ($LASTEXITCODE -ne 0) { Write-Host 'build failed'; exit 1 }
Write-Host 'build ok'
if ($BuildOnly) { exit 0 }

$dir = Join-Path 'outputs' $Name
New-Item -ItemType Directory -Force $dir | Out-Null
$sets = [ordered]@{
    a = '--saved --slide 76 --size 1920x1040'
    b = '--saved --slide 76 --best --size 1920x1040'
    c = '--curve --size 1920x1040'
    d = '--fit --size 1920x1040'
    e = '--joint --limit 400 --size 1920x1040'
    f = '--saved --slide 76 --size 1280x800'
    g = '--size 1920x1040'
}
$env:__COMPAT_LAYER = 'RunAsInvoker'
$stamp = Get-Date
$procs = foreach ($key in $sets.Keys) {
    Start-Process -FilePath $out -ArgumentList ("--simulate --snapshot $dir\$key.png " + $sets[$key]) -PassThru
}
$procs | Wait-Process -Timeout 120 -ErrorAction SilentlyContinue
$failed = 0
foreach ($p in $procs) { if (-not $p.HasExited) { $p.Kill(); $failed++; Write-Host 'a snapshot run did not finish' } }
foreach ($key in $sets.Keys) {
    $file = Get-Item (Join-Path $dir "$key.png") -ErrorAction SilentlyContinue
    if (-not $file -or $file.LastWriteTime -lt $stamp) { Write-Host "no snapshot: $key ($($sets[$key]))"; $failed++ }
}
if ($failed -gt 0) { exit 2 }
Write-Host "snapshots ok: $dir"
exit 0
