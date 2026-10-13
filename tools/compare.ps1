# Compares window snapshots of two check runs pixel by pixel: outputs\<A>\x.png against outputs\<B>\x.png.
# Prints, per picture, how many pixels differ and the box that holds them (the timer in the top bar always differs).
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File tools\compare.ps1 -A prof2 -B merged -Keys a,b,f,h
#   ... -Bands   also lists each run of differing rows with its x range
param([string]$A, [string]$B, [string[]]$Keys = @('a', 'b', 'f', 'h'), [switch]$Bands)

Add-Type -AssemblyName System.Drawing
$root = Split-Path -Parent $PSScriptRoot
$Keys = ($Keys -join ',') -split ','  # with -File the list arrives as one string
foreach ($key in $Keys) {
    $pa = Join-Path $root "outputs\$A\$key.png"
    $pb = Join-Path $root "outputs\$B\$key.png"
    if (-not (Test-Path $pa) -or -not (Test-Path $pb)) { Write-Host "$key : missing"; continue }
    $ia = New-Object System.Drawing.Bitmap $pa
    $ib = New-Object System.Drawing.Bitmap $pb
    if ($ia.Width -ne $ib.Width -or $ia.Height -ne $ib.Height) { Write-Host "$key : sizes differ"; $ia.Dispose(); $ib.Dispose(); continue }
    $rect = New-Object System.Drawing.Rectangle 0, 0, $ia.Width, $ia.Height
    $da = $ia.LockBits($rect, 'ReadOnly', 'Format32bppArgb')
    $db = $ib.LockBits($rect, 'ReadOnly', 'Format32bppArgb')
    $n = $da.Stride * $da.Height
    $ba = New-Object byte[] $n
    $bb = New-Object byte[] $n
    [Runtime.InteropServices.Marshal]::Copy($da.Scan0, $ba, 0, $n)
    [Runtime.InteropServices.Marshal]::Copy($db.Scan0, $bb, 0, $n)
    $ia.UnlockBits($da); $ib.UnlockBits($db)
    $count = 0; $x0 = [int]::MaxValue; $y0 = [int]::MaxValue; $x1 = -1; $y1 = -1
    $stride = $da.Stride
    $rows = @{}  # y -> leftmost and rightmost differing x of that row
    for ($i = 0; $i -lt $n; $i += 4) {
        if ($ba[$i] -ne $bb[$i] -or $ba[$i + 1] -ne $bb[$i + 1] -or $ba[$i + 2] -ne $bb[$i + 2]) {
            $count++
            $x = ($i % $stride) / 4; $y = [int][math]::Floor($i / $stride)
            if ($x -lt $x0) { $x0 = $x }; if ($x -gt $x1) { $x1 = $x }
            if ($y -lt $y0) { $y0 = $y }; if ($y -gt $y1) { $y1 = $y }
            if ($rows.ContainsKey($y)) { if ($x -lt $rows[$y][0]) { $rows[$y][0] = $x }; if ($x -gt $rows[$y][1]) { $rows[$y][1] = $x } }
            else { $rows[$y] = @($x, $x) }
        }
    }
    $ia.Dispose(); $ib.Dispose()
    if ($count -eq 0) { Write-Host "$key : identical"; continue }
    Write-Host "$key : $count pixels differ, box x $x0-$x1, y $y0-$y1"
    if (-not $Bands) { continue }
    # Runs of differing rows (gaps of up to 3 rows are bridged), each with its own x range: shows whether the
    # difference is one place or several far apart inside the box.
    $from = $null; $last = $null; $bx0 = 0; $bx1 = 0
    foreach ($y in ($rows.Keys | Sort-Object)) {
        if ($null -ne $from -and $y - $last -gt 4) { Write-Host "    rows $from-$last, x $bx0-$bx1"; $from = $null }
        if ($null -eq $from) { $from = $y; $bx0 = $rows[$y][0]; $bx1 = $rows[$y][1] }
        if ($rows[$y][0] -lt $bx0) { $bx0 = $rows[$y][0] }; if ($rows[$y][1] -gt $bx1) { $bx1 = $rows[$y][1] }
        $last = $y
    }
    if ($null -ne $from) { Write-Host "    rows $from-$last, x $bx0-$bx1" }
}
