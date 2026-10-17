# Installs the latest GANG release on Windows.
#
#   irm https://raw.githubusercontent.com/AyberkA0/gang/main/install.ps1 | iex
#
# For a private repository, set $env:GITHUB_TOKEN (or have the gh CLI signed in).
# $env:GANG_INSTALL_DIR chooses where gang.exe goes (default: %LOCALAPPDATA%\gang).
$ErrorActionPreference = "Stop"

$repo = "AyberkA0/gang"
$dir = if ($env:GANG_INSTALL_DIR) { $env:GANG_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA "gang" }
$asset = "gang-x86_64-pc-windows-msvc.zip"

$token = $env:GITHUB_TOKEN
if (-not $token -and (Get-Command gh -ErrorAction SilentlyContinue)) {
    $token = (gh auth token 2>$null)
}
$headers = @{ "User-Agent" = "gang-installer" }
if ($token) { $headers["Authorization"] = "Bearer $token" }

Write-Host "Finding the latest GANG release..."
$release = Invoke-RestMethod -Headers $headers "https://api.github.com/repos/$repo/releases/latest"
$found = $release.assets | Where-Object { $_.name -eq $asset } | Select-Object -First 1
if (-not $found) { throw "No $asset in the latest release." }

$tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("gang-" + [guid]::NewGuid())
New-Item -ItemType Directory -Path $tmp | Out-Null
try {
    $headers["Accept"] = "application/octet-stream"
    Invoke-WebRequest -Headers $headers -Uri $found.url -OutFile (Join-Path $tmp $asset)
    Expand-Archive -Path (Join-Path $tmp $asset) -DestinationPath $tmp
    New-Item -ItemType Directory -Force -Path $dir | Out-Null
    Move-Item -Force (Join-Path $tmp "gang.exe") (Join-Path $dir "gang.exe")
} finally {
    Remove-Item -Recurse -Force $tmp
}

$userPath = [Environment]::GetEnvironmentVariable("Path", "User")
if (($userPath -split ";") -notcontains $dir) {
    [Environment]::SetEnvironmentVariable("Path", "$userPath;$dir", "User")
    Write-Host "Added $dir to your PATH (open a new terminal to use it)."
}
Write-Host "Installed $(& (Join-Path $dir 'gang.exe') --version) to $dir"
