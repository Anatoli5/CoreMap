# Builds the new window's page (DECISIONS #27): the TypeScript compiler checks the sources in ui\src and writes
# JavaScript into page\js; ui\index.html, ui\style.css and the browser builds of Preact and its signals are copied
# into page\. No bundler: the page loads its modules itself and finds the libraries by the import map in index.html.
# The compiler is JavaScript run by node.exe - nothing native for Smart App Control to rate.
#
# page\ is build output and is not kept in git; it is rebuilt from scratch so that the JavaScript of a renamed or
# deleted source file cannot survive. The packages sit in node_modules\ (versions in package.json; how they were
# fetched without npm: AGENTS.md).
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File tools\page.ps1
#
# Exit code 0 = built; 1 = a tool or package is missing; 2 = the type check failed.
$root = Split-Path -Parent $PSScriptRoot

$node = (Get-Command node -ErrorAction SilentlyContinue | Select-Object -First 1).Source
if (-not $node) { $node = 'C:\Users\ramsp\.cache\codex-runtimes\codex-primary-runtime\dependencies\node\bin\node.exe' }
if (-not (Test-Path $node)) { Write-Host 'page: node.exe not found'; exit 1 }
$tsc = Join-Path $root 'node_modules\typescript\lib\tsc.js'
$libs = @(
    'preact\dist\preact.module.js',
    'preact\hooks\dist\hooks.module.js',
    'preact\jsx-runtime\dist\jsxRuntime.module.js',
    '@preact\signals\dist\signals.module.js',
    '@preact\signals-core\dist\signals-core.module.js'
)
foreach ($file in @($tsc) + ($libs | ForEach-Object { Join-Path $root "node_modules\$_" })) {
    if (-not (Test-Path $file)) { Write-Host "page: missing $file"; exit 1 }
}

$page = Join-Path $root 'page'
if (Test-Path $page) { Remove-Item -LiteralPath $page -Recurse -Force }
New-Item -ItemType Directory -Force (Join-Path $page 'lib') | Out-Null

& $node $tsc -p (Join-Path $root 'ui') | Out-Host
if ($LASTEXITCODE -ne 0) { Write-Host 'page: type check failed'; exit 2 }

Copy-Item (Join-Path $root 'ui\index.html'), (Join-Path $root 'ui\style.css') $page
foreach ($lib in $libs) { Copy-Item (Join-Path $root "node_modules\$lib") (Join-Path $page 'lib') }
Write-Host 'page ok'
exit 0
