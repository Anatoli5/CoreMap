# Delivery (DECISIONS #33): puts the checked build from build\ in place of the owner's programs in the project root.
# Run it only after tools\check.ps1 passed on the same sources. It refuses while any of the programs runs (Windows
# locks a running exe) and changes nothing then.
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File tools\deliver.ps1
#
# Version numbers: VERSION holds the number of the build in build\ (the one being delivered); archive\installed.txt
# holds the number now in the root. Each replaced program goes to archive\ as "<Name> <its number>.exe". After the
# delivery VERSION moves on to the next number, so no later build can carry a number the owner already has (global
# rule G39). CHANGELOG.md gets the delivered number by hand.
$root = Split-Path -Parent $PSScriptRoot
$build = Join-Path $root 'build'
$archive = Join-Path $root 'archive'
$installedFile = Join-Path $archive 'installed.txt'
$names = @('CoreMap.exe', 'CoreMapTray.exe', 'CoreMapWindow.exe')

$version = (Get-Content (Join-Path $root 'VERSION') -TotalCount 1).Trim()
$installed = if (Test-Path $installedFile) { (Get-Content $installedFile -TotalCount 1).Trim() } else { '' }
if ($installed -eq $version) { Write-Host "version $version is already delivered - raise VERSION and rebuild"; exit 3 }

foreach ($name in $names) {
    if (-not (Test-Path (Join-Path $build $name))) { Write-Host "build\$name is missing - run tools\check.ps1 first"; exit 2 }
}
# By process name, not by path: the logon program runs elevated, and Windows hides an elevated process's path from
# an unelevated one - matching by path missed the running CoreMapTray.exe on 15.10 and its file was replaced under it.
# A hidden check running from build\ has the same names and also blocks delivery; wait for it to end.
$plain = $names | ForEach-Object { [IO.Path]::GetFileNameWithoutExtension($_) }
$running = Get-Process | Where-Object { $plain -contains $_.Name }
if ($running) {
    Write-Host ('running now, close first: ' + (($running | ForEach-Object { $_.Name + '.exe' }) -join ', '))
    exit 1
}

New-Item -ItemType Directory -Force $archive | Out-Null
foreach ($name in $names) {
    $current = Join-Path $root $name
    if (Test-Path $current) {
        $label = if ($installed) { $installed } else { 'unnumbered' }
        $kept = Join-Path $archive ([IO.Path]::GetFileNameWithoutExtension($name) + " $label.exe")
        Move-Item $current $kept -Force
    }
    Copy-Item (Join-Path $build $name) $current
}
Set-Content -Path $installedFile -Value $version -Encoding ASCII

# Next number: the last part goes up by one (0.2 -> 0.3).
$parts = $version.Split('.')
$parts[$parts.Length - 1] = [string]([int]$parts[$parts.Length - 1] + 1)
$next = $parts -join '.'
Set-Content -Path (Join-Path $root 'VERSION') -Value $next -Encoding ASCII
Write-Host "delivered $version (previous $installed kept in archive); VERSION is now $next"
exit 0
