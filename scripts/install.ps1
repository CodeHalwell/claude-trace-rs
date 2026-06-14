# claude-trace-rs installer for Windows (PowerShell).
#
#   irm https://raw.githubusercontent.com/CodeHalwell/claude-trace-rs/main/scripts/install.ps1 | iex
#
# Downloads the latest release zip and installs claude-trace-rs.exe into
# %LOCALAPPDATA%\Programs\claude-trace-rs (added to your user PATH).
$ErrorActionPreference = 'Stop'

$repo = 'CodeHalwell/claude-trace-rs'
$bin  = 'claude-trace-rs'
$installDir = Join-Path $env:LOCALAPPDATA "Programs\$bin"
$target = 'x86_64-pc-windows-msvc'

Write-Host "==> Resolving latest release..." -ForegroundColor Blue
$rel = Invoke-RestMethod "https://api.github.com/repos/$repo/releases/latest"
$tag = $rel.tag_name
$version = $tag.TrimStart('v')
$asset = "$bin-$version-$target.zip"
$url = "https://github.com/$repo/releases/download/$tag/$asset"

$tmp = Join-Path $env:TEMP ([System.Guid]::NewGuid())
New-Item -ItemType Directory -Force -Path $tmp | Out-Null
try {
  Write-Host "==> Downloading $asset..." -ForegroundColor Blue
  Invoke-WebRequest -Uri $url -OutFile (Join-Path $tmp $asset)
  Expand-Archive -Path (Join-Path $tmp $asset) -DestinationPath $tmp -Force
  New-Item -ItemType Directory -Force -Path $installDir | Out-Null
  Copy-Item (Join-Path $tmp "$bin-$version-$target\$bin.exe") (Join-Path $installDir "$bin.exe") -Force

  $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
  if ($userPath -notlike "*$installDir*") {
    [Environment]::SetEnvironmentVariable('Path', "$userPath;$installDir", 'User')
    Write-Host "==> Added $installDir to your user PATH (restart your terminal)." -ForegroundColor Yellow
  }
  Write-Host "==> Installed $bin $version to $installDir" -ForegroundColor Green
  Write-Host "Run it with:  $bin serve --open"
} finally {
  Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
