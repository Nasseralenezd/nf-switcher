# Makes res/app.ico: the Segoe Fluent Icons "switch" glyph (E8AB) in white on an accent tile, PNG entries 16-256 px.
# Run: powershell -NoProfile -File tools/make-icon.ps1
Add-Type -AssemblyName System.Drawing
$out = Join-Path $PSScriptRoot '..\res\app.ico'
$sizes = 16, 20, 24, 32, 40, 48, 64, 256
$pngs = @()
foreach ($s in $sizes) {
  $bmp = New-Object System.Drawing.Bitmap $s, $s, ([System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
  $g = [System.Drawing.Graphics]::FromImage($bmp)
  $g.SmoothingMode = 'AntiAlias'
  $g.TextRenderingHint = 'AntiAliasGridFit'
  $g.Clear([System.Drawing.Color]::Transparent)
  $r = [Math]::Max(3, [int]($s * 0.22))
  $path = New-Object System.Drawing.Drawing2D.GraphicsPath
  $d = $r * 2
  $path.AddArc(0, 0, $d, $d, 180, 90)
  $path.AddArc($s - $d - 1, 0, $d, $d, 270, 90)
  $path.AddArc($s - $d - 1, $s - $d - 1, $d, $d, 0, 90)
  $path.AddArc(0, $s - $d - 1, $d, $d, 90, 90)
  $path.CloseFigure()
  $brush = New-Object System.Drawing.Drawing2D.LinearGradientBrush (New-Object System.Drawing.Point 0, 0), (New-Object System.Drawing.Point $s, $s), ([System.Drawing.Color]::FromArgb(255, 0, 120, 212)), ([System.Drawing.Color]::FromArgb(255, 0, 90, 170))
  $g.FillPath($brush, $path)
  $font = New-Object System.Drawing.Font 'Segoe Fluent Icons', ([single]($s * 0.56)), ([System.Drawing.FontStyle]::Regular), ([System.Drawing.GraphicsUnit]::Pixel)
  $fmt = New-Object System.Drawing.StringFormat
  $fmt.Alignment = 'Center'; $fmt.LineAlignment = 'Center'
  $rect = New-Object System.Drawing.RectangleF 0, ([single]($s * 0.02)), $s, $s
  $g.DrawString([string][char]0xE8AB, $font, [System.Drawing.Brushes]::White, $rect, $fmt)
  $g.Dispose()
  $ms = New-Object System.IO.MemoryStream
  $bmp.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
  $bmp.Dispose()
  $pngs += , $ms.ToArray()
}
$fs = [System.IO.File]::Create($out)
$w = New-Object System.IO.BinaryWriter $fs
$w.Write([UInt16]0); $w.Write([UInt16]1); $w.Write([UInt16]$sizes.Count)
$offset = 6 + 16 * $sizes.Count
for ($i = 0; $i -lt $sizes.Count; $i++) {
  $s = $sizes[$i]; $b = if ($s -ge 256) { 0 } else { $s }
  $w.Write([byte]$b); $w.Write([byte]$b); $w.Write([byte]0); $w.Write([byte]0)
  $w.Write([UInt16]1); $w.Write([UInt16]32)
  $w.Write([UInt32]$pngs[$i].Length); $w.Write([UInt32]$offset)
  $offset += $pngs[$i].Length
}
foreach ($p in $pngs) { $w.Write($p) }
$w.Close()
"wrote $out ($offset bytes)"
