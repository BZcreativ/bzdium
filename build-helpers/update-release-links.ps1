# Rewrites the README.md release links (tag, asset names, install commands)
# for a new version, then verifies every download URL is internally consistent.
#
# Why this exists: the bundler embeds the version in every asset name, and
# GitHub/Forgejo URLs embed both a tag and a file name. Bumping one half by
# hand is easy to miss - commit a8148d0 moved the tag to v0.2.14 but left six
# of seven file names at 0.2.13, so the install links 404'd. Run this instead:
#
#   .\build-helpers\update-release-links.ps1 -Version 0.2.15          # dry run
#   .\build-helpers\update-release-links.ps1 -Version 0.2.15 -Apply   # write
#
# The companion test (src-tauri/tests/readme_download_links.rs) enforces the
# same invariants on every cargo test.
param(
  [Parameter(Mandatory = $true)][string]$Version,
  [switch]$Apply
)
$ErrorActionPreference = "Stop"

if ($Version -notmatch '^\d+\.\d+\.\d+$') {
  throw "Version must look like 1.2.3, got '$Version'"
}

$readmePath = (Resolve-Path (Join-Path $PSScriptRoot "..\README.md")).Path
$text = [IO.File]::ReadAllText($readmePath)

# Single source of truth for the current version: the label the install
# section shows. Every artifact name and tag below is derived from it.
$label = [regex]::Match($text, 'Latest Release \(v(?<v>\d+\.\d+\.\d+)\)')
if (-not $label.Success) {
  throw "could not find 'Latest Release (vX.Y.Z)' in $readmePath"
}
$old = $label.Groups['v'].Value

if ($old -eq $Version) {
  Write-Host "README.md already points at v$Version - nothing to do."
  exit 0
}

# Exact-artifact matches only, so no history entry or pinned asset can be
# rewritten by accident.
$rewrites = [ordered]@{}
function Add-Rewrite([string]$From, [string]$To) { $script:rewrites[$From] = $To }
Add-Rewrite ("Latest Release (v{0})" -f $old)          ("Latest Release (v{0})" -f $Version)
Add-Rewrite ("releases/tag/v{0}" -f $old)              ("releases/tag/v{0}" -f $Version)
Add-Rewrite ("releases/download/v{0}/" -f $old)        ("releases/download/v{0}/" -f $Version)
Add-Rewrite ("bzdium_{0}_amd64.deb" -f $old)           ("bzdium_{0}_amd64.deb" -f $Version)
Add-Rewrite ("bzdium-{0}-1.x86_64.rpm" -f $old)        ("bzdium-{0}-1.x86_64.rpm" -f $Version)
Add-Rewrite ("bzdium_{0}_amd64.AppImage" -f $old)      ("bzdium_{0}_amd64.AppImage" -f $Version)
Add-Rewrite ("bzdium-{0}-linux-x64.tar.gz" -f $old)    ("bzdium-{0}-linux-x64.tar.gz" -f $Version)
Add-Rewrite ("bzdium-{0}-linux-x64.tgz" -f $old)       ("bzdium-{0}-linux-x64.tgz" -f $Version)
Add-Rewrite ("bzdium_{0}_x64-setup.exe" -f $old)       ("bzdium_{0}_x64-setup.exe" -f $Version)
Add-Rewrite ("bzdium-v{0}-x64-portable.exe" -f $old)   ("bzdium-v{0}-x64-portable.exe" -f $Version)

$total = 0
foreach ($key in $rewrites.Keys) {
  $count = ([regex]::Matches($text, [regex]::Escape($key))).Count
  if ($count -gt 0) {
    $text = $text.Replace($key, $rewrites[$key])
    $total += $count
    Write-Host ("{0,3}x  {1}" -f $count, $key)
  }
}
if ($total -eq 0) {
  throw "no release links matched '$old' - check the README install section"
}
Write-Host "replaced $total occurrence(s): v$old -> v$Version"

# Fail closed: every versioned asset URL must pair its tag with the same
# version embedded in the file name. The pinned Arch package (bzdium-bin-*)
# keeps its own tag and the demo video has no version, so the check follows
# the same exemption as the Rust test.
$problems = @()
foreach ($m in [regex]::Matches($text, 'releases/download/v(?<tag>\d+\.\d+\.\d+)/(?<file>[^)\s"]+)')) {
  $file = $m.Groups['file'].Value
  $fileVersion = [regex]::Match($file, '\d+\.\d+\.\d+')
  if ($fileVersion.Success -and $fileVersion.Value -ne $m.Groups['tag'].Value) {
    $problems += "v$($m.Groups['tag'].Value) + '$file'"
  }
}
if ($problems.Count -gt 0) {
  $detail = ($problems | ForEach-Object { "  $_" }) -join [Environment]::NewLine
  throw ("README.md would end up with mismatched download links:" + [Environment]::NewLine + $detail + [Environment]::NewLine + "Fix or remove those lines, then re-run.")
}

if (-not $Apply) {
  Write-Host "dry run - nothing written. Re-run with -Apply to update $readmePath."
  exit 0
}

[IO.File]::WriteAllText($readmePath, $text, (New-Object System.Text.UTF8Encoding($false)))
Write-Host "wrote $readmePath"
Write-Host "next: add a Release History entry for v$Version, then commit README.md."
