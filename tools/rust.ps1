# Gate for the Rust part (rust\AGENTS.md, "The gate"): format, lints, tests, advisories, release build; the built
# window is copied next to CoreMap.exe as CoreMapWindow.exe, where the engine check starts it.
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File tools\rust.ps1
#
# Exit code 0 = every step passed. Steps whose tool is not installed are reported as skipped, not passed.
# tools\check.ps1 calls this script in its own session, so the current folder is restored on every way out.
$root = Split-Path -Parent $PSScriptRoot
$env:PATH = (Join-Path $env:USERPROFILE '.cargo\bin') + ';' + $env:PATH

$steps = @(
    @('format', { cargo fmt --all --check }),
    @('lints', { cargo clippy --all-targets --release -- -D warnings }),
    @('tests', { cargo test --release })
)
if (Get-Command cargo-audit -ErrorAction SilentlyContinue) { $steps += , @('advisories', { cargo audit }) }
else { Write-Host 'rust: advisories SKIPPED - cargo-audit is not installed' }
$steps += , @('build', { cargo build --release })

$failed = $null
Push-Location (Join-Path $root 'rust')
try {
    foreach ($step in $steps) {
        Write-Host ('rust: ' + $step[0])
        & $step[1] | Out-Host
        if ($LASTEXITCODE -ne 0) { $failed = $step[0]; break }
    }
}
finally { Pop-Location }
if ($failed) { Write-Host "rust: $failed FAILED"; exit 2 }

Copy-Item (Join-Path $root 'rust\target\release\CoreMapWindow.exe') (Join-Path $root 'CoreMapWindow.exe') -Force
Write-Host 'rust ok'
exit 0
