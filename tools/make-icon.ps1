# Draws the CoreMap icon (a small die map: two columns of tiles from cool blue to hot red)
# and writes assets\coremap.ico with 16, 32, 48 and 256 pixel images.
# ASCII only: Windows PowerShell 5.1 reads a BOM-less script in the ANSI code page.
Add-Type -AssemblyName System.Drawing
$root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
$colors = '#1f6f9a', '#1f8f7a', '#3f9a3a', '#9a9a2a', '#b8662a', '#c23a3a'

function Draw([int]$size) {
    $bmp = New-Object Drawing.Bitmap $size, $size, ([Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [Drawing.Graphics]::FromImage($bmp)
    $g.Clear([Drawing.Color]::Transparent)
    $back = New-Object Drawing.SolidBrush ([Drawing.ColorTranslator]::FromHtml('#14161a'))
    $g.FillRectangle($back, 0, 0, $size, $size)
    $pad = [Math]::Max(1, [int]($size * 0.09)); $gap = [Math]::Max(1, [int]($size * 0.06))
    $w = [int](($size - 2 * $pad - $gap) / 2); $h = [int](($size - 2 * $pad - 2 * $gap) / 3)
    for ($i = 0; $i -lt 6; $i++) {
        $brush = New-Object Drawing.SolidBrush ([Drawing.ColorTranslator]::FromHtml($colors[$i]))
        $g.FillRectangle($brush, $pad + ($i % 2) * ($w + $gap), $pad + [Math]::Floor($i / 2) * ($h + $gap), $w, $h)
    }
    $g.Dispose()
    return $bmp
}

function PngBytes($bmp) {
    $ms = New-Object IO.MemoryStream
    $bmp.Save($ms, [Drawing.Imaging.ImageFormat]::Png)
    return , $ms.ToArray()
}

# Classic icon image: 40-byte header (height doubled), BGRA rows bottom-up, then an empty transparency mask.
function DibBytes($bmp) {
    $n = $bmp.Width
    $ms = New-Object IO.MemoryStream
    $w = New-Object IO.BinaryWriter $ms
    $w.Write([int]40); $w.Write([int]$n); $w.Write([int]($n * 2)); $w.Write([int16]1); $w.Write([int16]32)
    $w.Write([int]0); $w.Write([int]0); $w.Write([int]0); $w.Write([int]0); $w.Write([int]0); $w.Write([int]0)
    for ($y = $n - 1; $y -ge 0; $y--) { for ($x = 0; $x -lt $n; $x++) { $w.Write([int]$bmp.GetPixel($x, $y).ToArgb()) } }
    $maskRow = [int]([Math]::Ceiling($n / 32.0) * 4)
    $w.Write((New-Object byte[] ($maskRow * $n)))
    return , $ms.ToArray()
}

$images = @()
foreach ($size in 16, 32, 48, 256) {
    $bmp = Draw $size
    if ($size -eq 256) { $images += , @($size, (PngBytes $bmp)) } else { $images += , @($size, (DibBytes $bmp)) }
}

New-Item -ItemType Directory -Path (Join-Path $root 'assets') -Force | Out-Null
$out = New-Object IO.FileStream (Join-Path $root 'assets\coremap.ico'), 'Create'
$w = New-Object IO.BinaryWriter $out
$w.Write([int16]0); $w.Write([int16]1); $w.Write([int16]$images.Count)
$offset = 6 + 16 * $images.Count
foreach ($img in $images) {
    $size = $img[0]; $bytes = $img[1]
    $w.Write([byte]($size % 256)); $w.Write([byte]($size % 256)); $w.Write([byte]0); $w.Write([byte]0)
    $w.Write([int16]1); $w.Write([int16]32); $w.Write([int]$bytes.Length); $w.Write([int]$offset)
    $offset += $bytes.Length
}
foreach ($img in $images) { $w.Write($img[1]) }
$w.Close()
"written: " + (Join-Path $root 'assets\coremap.ico')
