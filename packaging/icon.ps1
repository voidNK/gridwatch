# Draws the gridwatch icon (a 3x3 board of status lights) and writes icon.png and icon.ico.
param([string]$OutDir)
Add-Type -AssemblyName System.Drawing

function New-RoundedPath([single]$x, [single]$y, [single]$w, [single]$h, [single]$r) {
    $p = New-Object System.Drawing.Drawing2D.GraphicsPath
    $d = $r * 2
    $p.AddArc($x, $y, $d, $d, 180, 90)
    $p.AddArc($x + $w - $d, $y, $d, $d, 270, 90)
    $p.AddArc($x + $w - $d, $y + $h - $d, $d, $d, 0, 90)
    $p.AddArc($x, $y + $h - $d, $d, $d, 90, 90)
    $p.CloseFigure()
    return $p
}

function New-Icon([int]$size) {
    $bmp = New-Object System.Drawing.Bitmap($size, $size, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = 'AntiAlias'
    $g.Clear([System.Drawing.Color]::Transparent)
    $s = $size / 256.0
    # Dark tile with a thin lighter edge.
    $tile = New-RoundedPath (8 * $s) (8 * $s) (240 * $s) (240 * $s) (52 * $s)
    $g.FillPath((New-Object System.Drawing.SolidBrush([System.Drawing.Color]::FromArgb(255, 26, 26, 25))), $tile)
    $g.DrawPath((New-Object System.Drawing.Pen([System.Drawing.Color]::FromArgb(255, 70, 70, 66), [single](4 * $s))), $tile)
    # Nine lights: most are fine, one warns, one is in trouble.
    $green = [System.Drawing.Color]::FromArgb(255, 34, 163, 90)
    $yellow = [System.Drawing.Color]::FromArgb(255, 250, 178, 25)
    $red = [System.Drawing.Color]::FromArgb(255, 221, 59, 59)
    $colors = @($green, $green, $green, $green, $yellow, $green, $green, $green, $red)
    for ($i = 0; $i -lt 9; $i++) {
        $col = $i % 3; $row = [math]::Floor($i / 3)
        $x = (44 + $col * 60) * $s; $y = (44 + $row * 60) * $s
        $light = New-RoundedPath $x $y (48 * $s) (48 * $s) (13 * $s)
        $g.FillPath((New-Object System.Drawing.SolidBrush($colors[$i])), $light)
    }
    $g.Dispose()
    return $bmp
}

$png = Join-Path $OutDir 'icon.png'
(New-Icon 512).Save($png, [System.Drawing.Imaging.ImageFormat]::Png)

# An .ico is a small directory followed by one PNG per size.
$sizes = 256, 64, 48, 32, 16
$images = foreach ($size in $sizes) {
    $ms = New-Object System.IO.MemoryStream
    (New-Icon $size).Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
    , $ms.ToArray()
}
$out = New-Object System.IO.MemoryStream
$w = New-Object System.IO.BinaryWriter($out)
$w.Write([uint16]0); $w.Write([uint16]1); $w.Write([uint16]$sizes.Count)
$offset = 6 + 16 * $sizes.Count
for ($i = 0; $i -lt $sizes.Count; $i++) {
    $dim = if ($sizes[$i] -ge 256) { 0 } else { $sizes[$i] }
    $w.Write([byte]$dim); $w.Write([byte]$dim); $w.Write([byte]0); $w.Write([byte]0)
    $w.Write([uint16]1); $w.Write([uint16]32)
    $w.Write([uint32]$images[$i].Length); $w.Write([uint32]$offset)
    $offset += $images[$i].Length
}
foreach ($image in $images) { $w.Write($image) }
[System.IO.File]::WriteAllBytes((Join-Path $OutDir 'icon.ico'), $out.ToArray())
"wrote $png and icon.ico"
