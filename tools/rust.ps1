# Gate for the Rust part (rust\AGENTS.md, "The gate"): format, lints, tests, advisories, release build; the built
# window is copied next to CoreMap.exe as CoreMapWindow.exe, where the engine check starts it; the logon program as
# CoreMapTrayNew.exe, where the logon check (tools\check.ps1) starts it - CoreMapTray.exe may be running on the owner's
# machine, so a check never replaces it; swapping it in is a delivery step, like CoreMapNew.exe for CoreMap.exe.
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File tools\rust.ps1
#
# Exit code 0 = every step passed. Steps whose tool is not installed are reported as skipped, not passed.
# tools\check.ps1 calls this script in its own session, so the current folder is restored on every way out.
#
# Build stamp (rust\window\build.rs, rust\tray\build.rs): the build time for the window title and the logon log. It is
# renewed when a file under rust\ (outside target\) changed, and again when Smart App Control refuses the fresh exe: a rebuild of unchanged Rust code is
# the same file and would get the same verdict, so the stamp also carries a tick count that makes the file differ.
$root = Split-Path -Parent $PSScriptRoot
$env:PATH = (Join-Path $env:USERPROFILE '.cargo\bin') + ';' + $env:PATH
$stampFile = Join-Path $root 'rust\target\coremap-stamp.txt'
$exes = [ordered]@{ 'CoreMapWindow.exe' = 'CoreMapWindow.exe'; 'CoreMapTray.exe' = 'CoreMapTrayNew.exe' }

function NewStamp {
    $text = (Get-Date -Format 'dd.MM.yy HH:mm') + '|' + [DateTime]::Now.Ticks
    New-Item -ItemType Directory -Force (Split-Path -Parent $stampFile) | Out-Null
    Set-Content -Path $stampFile -Value $text -Encoding ASCII
    return $text
}

$newest = (Get-ChildItem (Join-Path $root 'rust') -Recurse -File | Where-Object { $_.FullName -notmatch '\\target\\' } | Sort-Object LastWriteTime | Select-Object -Last 1).LastWriteTime
if ((Test-Path $stampFile) -and (Get-Item $stampFile).LastWriteTime -ge $newest) { $env:COREMAP_STAMP = (Get-Content $stampFile -TotalCount 1) }
else { $env:COREMAP_STAMP = NewStamp }

function Step($name, [scriptblock]$run) {
    Write-Host ('rust: ' + $name)
    & $run | Out-Host
    return $LASTEXITCODE -eq 0
}

$failed = $null
Push-Location (Join-Path $root 'rust')
try {
    if (-not (Step 'format' { cargo fmt --all --check })) { $failed = 'format' }
    elseif (-not (Step 'lints' { cargo clippy --all-targets --release -- -D warnings })) { $failed = 'lints' }
    elseif (-not (Step 'tests' { cargo test --release })) { $failed = 'tests' }
    elseif ((Get-Command cargo-audit -ErrorAction SilentlyContinue) -and -not (Step 'advisories' { cargo audit })) { $failed = 'advisories' }
    else {
        if (-not (Get-Command cargo-audit -ErrorAction SilentlyContinue)) { Write-Host 'rust: advisories SKIPPED - cargo-audit is not installed' }
        for ($attempt = 1; $attempt -le 4; $attempt++) {
            if (-not (Step 'build' { cargo build --release })) { $failed = 'build'; break }
            try {
                # Each exe is copied and probe-started (--probe returns before anything else, no rights asked).
                foreach ($name in $exes.Keys) {
                    $exe = Join-Path $root $exes[$name]
                    Copy-Item (Join-Path $root ("rust\target\x86_64-pc-windows-msvc\release\" + $name)) $exe -Force
                    $probe = Start-Process -FilePath $exe -ArgumentList '--probe' -PassThru -Wait -ErrorAction Stop
                    if ($probe.ExitCode -ne 0) { $failed = "$name probe run, exit code " + $probe.ExitCode }
                }
                break
            }
            catch {
                Write-Host ('rust: start refused: ' + $_.Exception.Message)
                if ($attempt -eq 4) { $failed = 'a Rust exe was refused four times'; break }
                $env:COREMAP_STAMP = NewStamp
                Write-Host "rust: rebuilding with a new stamp, attempt $($attempt + 1)"
            }
        }
    }
}
finally { Pop-Location }
if ($failed) { Write-Host "rust: $failed FAILED"; exit 2 }
Write-Host ('rust ok, build ' + $env:COREMAP_STAMP)
exit 0
