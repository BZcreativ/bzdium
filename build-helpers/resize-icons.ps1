# Resizes branding-src/app-icon-1024.png to the ICO size set.
# Output: branding-src/gen/<size>.png + gen-icon-32.png for the tray.
param(
  [string]$Src = "$PSScriptRoot\..\branding-src\app-icon-1024.png",
  [string]$OutDir = "$PSScriptRoot\..\branding-src\gen"
)
$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.Drawing

New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$srcImg = [System.Drawing.Image]::FromFile((Resolve-Path $Src))
try {
  foreach ($size in 16, 24, 32, 48, 64, 128, 256) {
    $bmp = New-Object System.Drawing.Bitmap $size, $size
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
    $g.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
    $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::HighQuality
    $g.DrawImage($srcImg, 0, 0, $size, $size)
    $g.Dispose()
    $bmp.Save((Join-Path $OutDir "$size.png"), [System.Drawing.Imaging.ImageFormat]::Png)
    $bmp.Dispose()
    Write-Host "resized $size"
  }
} finally {
  $srcImg.Dispose()
}
Write-Host "done -> $OutDir"
