<#
  Builds the Microsoft Store package (MSIX) of Wiqo Desktop for one Windows architecture, on Windows.

    pwsh desktop/scripts/build-msix.ps1 -Target x86_64-pc-windows-msvc  -Out dist
    pwsh desktop/scripts/build-msix.ps1 -Target aarch64-pc-windows-msvc -Out dist

  Needs: Rust with the target, `cargo install tauri-cli --version "^2"`, the Windows SDK (makeappx.exe) and, for the full package, the helper that drives
  other apps (scripts/build-helper.sh <target> desktop/app/binaries). The program is built without the NSIS installer (`--no-bundle`).

  The package is NOT signed: Partner Center signs what it publishes. To try it on a PC first, sign it with a test certificate whose subject is the
  Publisher in windows/msix/AppxManifest.xml.

  The Store wants a four-part version whose first part is not 0 and whose last part is 0, so Wiqo 0.1.6 ships as 1.1.6.0 (major + 1, minor, patch, 0).
#>
param(
  [Parameter(Mandatory = $true)][ValidateSet('x86_64-pc-windows-msvc', 'aarch64-pc-windows-msvc')][string]$Target,
  [string]$Out = 'dist'
)
$ErrorActionPreference = 'Stop'

$desktop = Resolve-Path (Join-Path $PSScriptRoot '..')
$app = Join-Path $desktop 'app'
$arch = if ($Target -like 'aarch64*') { 'arm64' } else { 'x64' }

$config = Get-Content (Join-Path $app 'tauri.conf.json') -Raw | ConvertFrom-Json
$parts = $config.version.Split('.')
if ($parts.Count -ne 3) { throw "Unexpected app version: $($config.version)" }
$packageVersion = '{0}.{1}.{2}.0' -f ([int]$parts[0] + 1), [int]$parts[1], [int]$parts[2]

Push-Location $app
try {
  cargo tauri build --target $Target --no-bundle
  if ($LASTEXITCODE -ne 0) { throw 'Could not build Wiqo.exe.' }
} finally { Pop-Location }

$release = Join-Path $desktop "target/$Target/release"
$exe = Join-Path $release 'Wiqo.exe'
if (-not (Test-Path $exe)) { throw "Missing $exe" }

$stage = Join-Path ([IO.Path]::GetTempPath()) "wiqo-msix-$arch"
if (Test-Path $stage) { Remove-Item $stage -Recurse -Force }
New-Item -ItemType Directory -Path $stage | Out-Null
Copy-Item $exe $stage

# The helper that drives other apps' controls. WIQO_HELPER points at the built (and, for a release, signed) program.
$helper = if ($env:WIQO_HELPER) { $env:WIQO_HELPER } else { Join-Path $app 'binaries/agent-desktop.exe' }
if (Test-Path $helper) {
  Copy-Item $helper (Join-Path $stage 'agent-desktop.exe')
  $license = Join-Path (Split-Path $helper) 'agent-desktop.LICENSE'
  if (Test-Path $license) { Copy-Item $license $stage }
} else {
  Write-Warning 'No agent-desktop.exe: this package cannot drive other apps.'
}

# Wiqo's own browser: the Playwright worker, run with the computer's Node.js (beside Wiqo.exe, where browser.rs looks).
$worker = if ($env:WIQO_BROWSER_WORKER_DIR) { $env:WIQO_BROWSER_WORKER_DIR } else { Join-Path $desktop 'browser-worker' }
if (Test-Path (Join-Path $worker 'index.mjs')) {
  $workerStage = Join-Path $stage 'browser-worker'
  New-Item -ItemType Directory -Path $workerStage | Out-Null
  foreach ($name in 'index.mjs', 'downloads.mjs', 'package.json', 'package-lock.json') { Copy-Item (Join-Path $worker $name) $workerStage }
}

Copy-Item (Join-Path $app 'windows/msix/Assets') (Join-Path $stage 'Assets') -Recurse
$manifest = (Get-Content (Join-Path $app 'windows/msix/AppxManifest.xml') -Raw).Replace('@VERSION@', $packageVersion).Replace('@ARCH@', $arch)
Set-Content -Path (Join-Path $stage 'AppxManifest.xml') -Value $manifest -Encoding utf8

$makeappx = Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\bin\*\x64\makeappx.exe" -ErrorAction SilentlyContinue |
  Sort-Object { [version]$_.Directory.Parent.Name } -Descending | Select-Object -First 1
if (-not $makeappx) { throw 'makeappx.exe not found: install the Windows SDK.' }

New-Item -ItemType Directory -Path $Out -Force | Out-Null
$package = Join-Path (Resolve-Path $Out) "Wiqo_${packageVersion}_$arch.msix"
& $makeappx.FullName pack /d $stage /p $package /o
if ($LASTEXITCODE -ne 0) { throw 'makeappx could not pack the package.' }

# Check what ended up inside: identity, architecture and the programs the app looks for next to Wiqo.exe.
$listing = & $makeappx.FullName unpack /p $package /d (Join-Path ([IO.Path]::GetTempPath()) "wiqo-msix-check-$arch") /o 2>&1
$check = Join-Path ([IO.Path]::GetTempPath()) "wiqo-msix-check-$arch"
$packed = [xml](Get-Content (Join-Path $check 'AppxManifest.xml') -Raw)
if ($packed.Package.Identity.Name -ne 'OsmanEmreKoca.Wiqo') { throw 'Package name does not match Partner Center.' }
if ($packed.Package.Identity.ProcessorArchitecture -ne $arch) { throw 'Package architecture is wrong.' }
if (-not (Test-Path (Join-Path $check 'Wiqo.exe'))) { throw 'Wiqo.exe is missing from the package.' }
Remove-Item $check -Recurse -Force

Write-Host "Built $package (version $packageVersion, $arch)"
