# Project check for the agent: builds the sources into CoreMapNew.exe (the owner's CoreMap.exe is not touched),
# takes hidden simulated window snapshots into outputs\<Name>\, runs the self-test of the measurement base and the
# hidden check of the tray resident. Nothing appears on screen, no elevation prompt, no real load. Exit code 0 =
# built, every snapshot was written, the self-test and the resident check passed.
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File tools\check.ps1 -Name stage1
#   powershell -NoProfile -ExecutionPolicy Bypass -File tools\check.ps1 -BuildOnly
#
# Compare the pictures with those of an earlier run: tools\compare.ps1 -A <earlier> -B <Name>. Sets a, b, f, h
# show no running measurement and must match pixel for pixel (the minutes in h aside); c, d, e, g are simulated
# measurements in progress and are compared by eye.
#
# Smart App Control rates every fresh unsigned build anew and now and then blocks one ("An Application Control
# policy has blocked this file"); the same sources built again usually pass. So a blocked build is rebuilt, up to
# four times.
param([string]$Name = 'check', [switch]$BuildOnly)

$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

# The owner's own CoreMap shows " - " with an em dash in its window title while a real measurement runs.
# Builds and hidden runs heat the processor and spoil that measurement, so the check waits for it to end.
$waited = 0
while ($true) {
    $busy = Get-Process CoreMap -ErrorAction SilentlyContinue | Where-Object { $_.MainWindowTitle -match [string][char]0x2014 }
    if (-not $busy) { break }
    if ($waited -eq 0) { Write-Host 'the owner is measuring - waiting for it to end' }
    if ($waited -ge 3600) { Write-Host 'the owner is still measuring after an hour - not started'; exit 5 }
    Start-Sleep -Seconds 30
    $waited += 30
}

# A string literal broken by an automatic replacement leaves a line that is only: " +
$broken = Select-String -Path src\*.cs -Pattern '^\s*" \+\s*$'
if ($broken) { $broken | ForEach-Object { Write-Host ("broken literal: " + $_.Path + ":" + $_.LineNumber) }; exit 3 }

$csc = Join-Path $env:WINDIR 'Microsoft.NET\Framework64\v4.0.30319\csc.exe'
$out = Join-Path $root 'CoreMapNew.exe'
$icon = @()
if (Test-Path assets\coremap.ico) { $icon = @('/win32icon:assets\coremap.ico') }

function Build {
    # The compiler's messages go to the host, not into the function's result: with them in the result a failed
    # build returned a non-empty list, which counts as true, and the check went on with the previous exe.
    & $csc /nologo /codepage:65001 /target:winexe /optimize+ /platform:x64 /win32manifest:app.manifest @icon `
        /r:System.Windows.Forms.dll /r:System.Drawing.dll "/out:$out" src\*.cs | Out-Host
    return $LASTEXITCODE -eq 0
}

if (-not (Build)) { Write-Host 'build failed'; exit 1 }
Write-Host 'build ok'
if ($BuildOnly) { exit 0 }

$dir = Join-Path 'outputs' $Name
New-Item -ItemType Directory -Force $dir | Out-Null
# The simulated load on a whole E-core cluster reads far above 100 C, and the overheat guard would stop the
# measurement; d and e raise the guard limit to show the measurement itself.
$sets = [ordered]@{
    a = '--saved --slide 76 --size 1920x1040'
    b = '--saved --slide 76 --best --size 1920x1040'
    c = '--curve --size 1920x1040'
    d = '--fit --limit 400 --size 1920x1040'
    e = '--joint --limit 400 --size 1920x1040'
    f = '--saved --slide 76 --size 1280x800'
    g = '--size 1920x1040'
    h = '--profile --saved --size 1920x1040'
}
$env:__COMPAT_LAYER = 'RunAsInvoker'
$stamp = Get-Date
$procs = $null
for ($attempt = 1; $attempt -le 4; $attempt++) {
    $procs = @()
    $blocked = $false
    foreach ($key in $sets.Keys) {
        try {
            $procs += Start-Process -FilePath $out -ArgumentList ("--simulate --snapshot $dir\$key.png " + $sets[$key]) -PassThru -ErrorAction Stop
        }
        catch { $blocked = $true; Write-Host ("start refused: " + $_.Exception.Message); break }
    }
    if (-not $blocked) { break }
    foreach ($p in $procs) { if (-not $p.HasExited) { $p.Kill() } }
    $procs = $null
    Start-Sleep -Seconds 2
    if (-not (Build)) { Write-Host 'build failed'; exit 1 }
    Write-Host "rebuilt, attempt $($attempt + 1)"
}
if ($null -eq $procs) { Write-Host 'the build was refused four times'; exit 4 }

# Self-test of the measurement base (src\SelfTest.cs): no window, simulated limits, files go to a temp folder.
# It checks that every way out of a measurement puts the multiplier limits back. First line of the report: ok / FAILED.
$report = Join-Path $dir 'selftest.txt'
$procs += Start-Process -FilePath $out -ArgumentList "--simulate --snapshot $report --selftest" -PassThru

# The resident (src\Tray.cs, CoreMap.exe --tray) on simulated limits and a temp copy of the saved profiles: no tray
# icon, it applies the start-up profile, clicks through its menu, measures its memory and idle CPU time and writes
# a text report. First line of the report: ok / FAILED.
$tray = Join-Path $dir 'tray.txt'
$procs += Start-Process -FilePath $out -ArgumentList "--simulate --tray --saved --snapshot $tray" -PassThru

# Hover on the curve graph without a mouse (MainForm.HoverCheck): the cursor is put on every point twice, with a
# repaint after each; the highlighted block must not change, the point must stay under the cursor and have a tooltip.
# First line of the report: ok / FAILED.
$hover = Join-Path $dir 'hover.txt'
$procs += Start-Process -FilePath $out -ArgumentList "--simulate --saved --hover --snapshot $hover --size 1920x1040" -PassThru

$procs | Wait-Process -Timeout 120 -ErrorAction SilentlyContinue
$failed = 0
foreach ($p in $procs) { if (-not $p.HasExited) { $p.Kill(); $failed++; Write-Host 'a snapshot run did not finish' } }
foreach ($key in $sets.Keys) {
    $file = Get-Item (Join-Path $dir "$key.png") -ErrorAction SilentlyContinue
    if (-not $file -or $file.LastWriteTime -lt $stamp) { Write-Host "no snapshot: $key ($($sets[$key]))"; $failed++ }
}
$tested = Get-Item $report -ErrorAction SilentlyContinue
if (-not $tested -or $tested.LastWriteTime -lt $stamp) { Write-Host 'no self-test report'; $failed++ }
elseif ((Get-Content $report -TotalCount 1) -ne 'ok') {
    Write-Host "self-test failed, see $report"
    Select-String -Path $report -Pattern '^FAILED  ' -Encoding UTF8 | ForEach-Object { Write-Host $_.Line }
    $failed++
}
$trayed = Get-Item $tray -ErrorAction SilentlyContinue
if (-not $trayed -or $trayed.LastWriteTime -lt $stamp) { Write-Host 'no resident report'; $failed++ }
elseif ((Get-Content $tray -TotalCount 1) -ne 'ok') {
    Write-Host "resident check failed, see $tray"
    Select-String -Path $tray -Pattern '^FAILED' -Encoding UTF8 | ForEach-Object { Write-Host $_.Line }
    $failed++
}
$hovered = Get-Item $hover -ErrorAction SilentlyContinue
if (-not $hovered -or $hovered.LastWriteTime -lt $stamp) { Write-Host 'no hover report'; $failed++ }
elseif ((Get-Content $hover -TotalCount 1) -ne 'ok') {
    Write-Host "hover check failed, see $hover"
    Get-Content $hover -Encoding UTF8 -TotalCount 6 | ForEach-Object { Write-Host $_ }
    $failed++
}
if ($failed -gt 0) { exit 2 }
Write-Host "snapshots, self-test, resident and hover checks ok: $dir"
exit 0
