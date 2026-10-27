# Windows packages from the release binary (run on Windows; not tested here):
#   pwsh scripts/bundle-windows.ps1 [-NoBuild] [-NoInstaller]
# * target\bundle\<APP_ID>-<version>-windows-x64.zip   (the .exe + LICENSE)
# * target\bundle\<APP_ID>-<version>-windows-x64-setup.exe  (Inno Setup 6, `iscc` on PATH
#   or in Program Files; preinstalled on GitHub's windows runners)
# The .exe already carries the icon and version info (crates/music-gpui/build.rs).
param([switch]$NoBuild, [switch]$NoInstaller)
$ErrorActionPreference = "Stop"
$Root = Resolve-Path "$PSScriptRoot\.."
Set-Location $Root

$envFile = @{}
Get-Content packaging\app.env | Where-Object { $_ -notmatch '^\s*(#|$)' } | ForEach-Object {
  $k, $v = $_ -split '=', 2
  $envFile[$k.Trim()] = $v.Trim().Trim('"')
}
$version = (Select-String -Path Cargo.toml -Pattern '^version\s*=\s*"(.+)"' | Select-Object -First 1).Matches[0].Groups[1].Value
$appId = $envFile["APP_ID"]

if (-not $NoBuild) { cargo build --release -p music-gpui; if ($LASTEXITCODE) { exit $LASTEXITCODE } }
$exe = "target\release\music-gpui.exe"
$out = "target\bundle"
New-Item -ItemType Directory -Force $out | Out-Null

$stage = Join-Path $out "$appId-$version-windows-x64"
Remove-Item -Recurse -Force $stage -ErrorAction SilentlyContinue
New-Item -ItemType Directory $stage | Out-Null
Copy-Item $exe (Join-Path $stage "$appId.exe")
Copy-Item LICENSE $stage
Compress-Archive -Force -Path "$stage\*" -DestinationPath "$stage.zip"
Write-Host "built $stage.zip"

if (-not $NoInstaller) {
  $iscc = (Get-Command iscc -ErrorAction SilentlyContinue).Source
  if (-not $iscc) { $iscc = "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe" }
  & $iscc "/DAppName=$($envFile['APP_NAME'])" "/DAppId=$appId" "/DAppVersion=$version" `
    "/DAppSummary=$($envFile['APP_SUMMARY'])" "/DSourceExe=$Root\$exe" "/DOutputDir=$Root\$out" `
    packaging\windows\installer.iss
  if ($LASTEXITCODE) { exit $LASTEXITCODE }
}
