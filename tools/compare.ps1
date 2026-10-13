# Compares window snapshots of two check runs pixel by pixel: outputs\<A>\x.png against outputs\<B>\x.png.
# Prints, per picture, how many pixels differ and the box that holds them (the timer in the top bar always differs).
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File tools\compare.ps1 -A prof2 -B merged -Keys a,b,f,h
param([string]$A, [string]$B, [string[]]$Keys = @('a', 'b', 'f', 'h'))

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
    for ($i = 0; $i -lt $n; $i += 4) {
        if ($ba[$i] -ne $bb[$i] -or $ba[$i + 1] -ne $bb[$i + 1] -or $ba[$i + 2] -ne $bb[$i + 2]) {
            $count++
            $x = ($i % $stride) / 4; $y = [math]::Floor($i / $stride)
            if ($x -lt $x0) { $x0 = $x }; if ($x -gt $x1) { $x1 = $x }
            if ($y -lt $y0) { $y0 = $y }; if ($y -gt $y1) { $y1 = $y }
        }
    }
    $ia.Dispose(); $ib.Dispose()
    if ($count -eq 0) { Write-Host "$key : identical" }
    else { Write-Host "$key : $count pixels differ, box x $x0-$x1, y $y0-$y1" }
}
