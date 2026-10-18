# Makes the icons: the Segoe Fluent Icons "switch" glyph (E8AB) on no background, PNG entries.
#   res/app.ico         the app (Start, Installed apps, Explorer): blue, which reads on light and dark
#   res/tray-dark.ico   the tray icon on a dark taskbar: white, like Windows' own tray icons
#   res/tray-light.ico  the tray icon on a light taskbar: near black
# Run: powershell -NoProfile -File tools/make-icon.ps1
Add-Type -AssemblyName System.Drawing
$res = Join-Path $PSScriptRoot '..\res'

# One size: the glyph at its design size (em = icon size), centred on whole pixels so its strokes stay sharp.
function Glyph([int]$s, [scriptblock]$brushFor) {
  $bmp = New-Object System.Drawing.Bitmap $s, $s, ([System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
  $g = [System.Drawing.Graphics]::FromImage($bmp)
  $g.SmoothingMode = 'AntiAlias'
  # Pixel (x, y) covers x..x+1: without this GDI+ centres pixels on whole numbers, and every 1 px stroke turns
  # into two half-transparent rows.
  $g.PixelOffsetMode = 'Half'
  $g.Clear([System.Drawing.Color]::Transparent)
  $family = New-Object System.Drawing.FontFamily 'Segoe Fluent Icons'
  $path = New-Object System.Drawing.Drawing2D.GraphicsPath
  $path.AddString([string][char]0xE8AB, $family, 0, [single]$s, (New-Object System.Drawing.PointF 0, 0), [System.Drawing.StringFormat]::GenericTypographic)
  $b = $path.GetBounds()
  $m = New-Object System.Drawing.Drawing2D.Matrix
  $m.Translate([Math]::Round(($s - $b.Width) / 2 - $b.X), [Math]::Round(($s - $b.Height) / 2 - $b.Y))
  $path.Transform($m)
  $brush = & $brushFor $path.GetBounds()
  $g.FillPath($brush, $path)
  $brush.Dispose(); $path.Dispose(); $g.Dispose()
  $ms = New-Object System.IO.MemoryStream
  $bmp.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
  $bmp.Dispose()
  , $ms.ToArray()
}

function WriteIco([string]$out, [int[]]$sizes, [scriptblock]$brushFor) {
  $pngs = @()
  foreach ($s in $sizes) { $pngs += , (Glyph $s $brushFor) }
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
}

# The app: Windows' accent blue, lighter at the top (Fluent style), so it holds up on light and dark.
WriteIco (Join-Path $res 'app.ico') @(16, 20, 24, 32, 40, 48, 64, 256) {
  param($r)
  New-Object System.Drawing.Drawing2D.LinearGradientBrush (New-Object System.Drawing.PointF 0, ($r.Top - 1)), (New-Object System.Drawing.PointF 0, ($r.Bottom + 1)), ([System.Drawing.Color]::FromArgb(255, 41, 151, 255)), ([System.Drawing.Color]::FromArgb(255, 0, 103, 192))
}
# The tray: the small icon at every scaling from 100% to 400%, so Windows never has to resize one.
$tray = @(16, 20, 24, 28, 32, 36, 40, 48, 56, 64)
WriteIco (Join-Path $res 'tray-dark.ico') $tray { New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::White) }
WriteIco (Join-Path $res 'tray-light.ico') $tray { New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(255, 27, 27, 27)) }
