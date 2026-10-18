# Makes the README's pictures in docs/images: the flyout and the installer as `--shots` renders them offscreen
# (sample data, dark and light), with Windows 11's rounded corners, a thin border and a soft shadow, plus icon.png
# from res/app.ico. Nothing is shown on screen.
# Run after `cargo build --offline`: powershell -NoProfile -File tools/readme-images.ps1
param([string]$Exe = (Join-Path $PSScriptRoot '..\target\debug\nf-switcher.exe'))
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
$root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$out = Join-Path $root 'docs\images'
$shots = Join-Path ([IO.Path]::GetTempPath()) 'nf-switcher-readme'
Remove-Item $shots -Recurse -Force -ErrorAction SilentlyContinue
& $Exe --shots $shots | Out-Null
New-Item -ItemType Directory -Force $out | Out-Null

function RoundRect([single]$x, [single]$y, [single]$w, [single]$h, [single]$r) {
  $p = New-Object System.Drawing.Drawing2D.GraphicsPath
  $d = $r * 2
  $p.AddArc($x, $y, $d, $d, 180, 90)
  $p.AddArc($x + $w - $d, $y, $d, $d, 270, 90)
  $p.AddArc($x + $w - $d, $y + $h - $d, $d, $d, 0, 90)
  $p.AddArc($x, $y + $h - $d, $d, $d, 90, 90)
  $p.CloseFigure()
  $p
}

# The shots are at 2x, so 16 px corners and a 2 px border are Windows 11's 8 px and 1 px.
function Frame([string]$src, [string]$dst, [System.Drawing.Color]$border) {
  $img = [System.Drawing.Image]::FromFile($src)
  $m = 40; $r = 16; $w = $img.Width; $h = $img.Height
  $bmp = New-Object System.Drawing.Bitmap ($w + 2 * $m), ($h + 2 * $m), ([System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
  $g = [System.Drawing.Graphics]::FromImage($bmp)
  $g.SmoothingMode = 'AntiAlias'
  $g.Clear([System.Drawing.Color]::Transparent)
  # A soft shadow from stacked faint shapes, a little below the window.
  $shade = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(2, 0, 0, 0))
  for ($k = 28; $k -ge 1; $k--) {
    $g.FillPath($shade, (RoundRect ($m - $k) ($m - $k + 8) ($w + 2 * $k) ($h + 2 * $k) ($r + $k)))
  }
  $tex = New-Object System.Drawing.TextureBrush $img
  $tex.TranslateTransform($m, $m)
  $g.FillPath($tex, (RoundRect $m $m $w $h $r))
  $pen = New-Object System.Drawing.Pen $border, 2
  $g.DrawPath($pen, (RoundRect ($m + 1) ($m + 1) ($w - 2) ($h - 2) ($r - 1)))
  $g.Dispose(); $tex.Dispose(); $shade.Dispose(); $pen.Dispose(); $img.Dispose()
  $bmp.Save($dst, [System.Drawing.Imaging.ImageFormat]::Png)
  $bmp.Dispose()
}

$borders = @{ dark = [System.Drawing.Color]::FromArgb(66, 66, 66); light = [System.Drawing.Color]::FromArgb(213, 213, 213) }
foreach ($name in 'hero', 'add', 'settings', 'setup-options') {
  foreach ($theme in 'dark', 'light') {
    Frame (Join-Path $shots "$name-$theme.png") (Join-Path $out "$name-$theme.png") $borders[$theme]
  }
}

# icon.png: the icon's 256 px entry (stored as PNG; its size byte is 0).
$ico = [IO.File]::ReadAllBytes((Join-Path $root 'res\app.ico'))
for ($i = 0; $i -lt [BitConverter]::ToUInt16($ico, 4); $i++) {
  $e = 6 + 16 * $i
  if ($ico[$e] -eq 0) {
    $len = [BitConverter]::ToUInt32($ico, $e + 8); $off = [BitConverter]::ToUInt32($ico, $e + 12)
    [IO.File]::WriteAllBytes((Join-Path $out 'icon.png'), [byte[]]$ico[$off..($off + $len - 1)])
  }
}
Get-ChildItem $out | ForEach-Object { "{0,-26} {1,8}" -f $_.Name, $_.Length }
