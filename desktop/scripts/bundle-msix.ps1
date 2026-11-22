<#
  Joins the x64 and ARM64 packages from build-msix.ps1 into one .msixbundle, the file to upload in Partner Center (Packages).

    pwsh desktop/scripts/bundle-msix.ps1 -Dir dist
#>
param([string]$Dir = 'dist')
$ErrorActionPreference = 'Stop'

$packages = @(Get-ChildItem $Dir -Filter 'Wiqo_*_*.msix' -File)
if ($packages.Count -ne 2) { throw "Expected the x64 and arm64 packages in $Dir, found $($packages.Count)." }
$version = ($packages[0].BaseName -split '_')[1]
foreach ($package in $packages) {
  if (($package.BaseName -split '_')[1] -ne $version) { throw 'The two packages have different versions.' }
}

$makeappx = Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\bin\*\x64\makeappx.exe" -ErrorAction SilentlyContinue |
  Sort-Object { [version]$_.Directory.Parent.Name } -Descending | Select-Object -First 1
if (-not $makeappx) { throw 'makeappx.exe not found: install the Windows SDK.' }

$stage = Join-Path ([IO.Path]::GetTempPath()) 'wiqo-msix-bundle'
if (Test-Path $stage) { Remove-Item $stage -Recurse -Force }
New-Item -ItemType Directory -Path $stage | Out-Null
$packages | Copy-Item -Destination $stage

$bundle = Join-Path (Resolve-Path $Dir) "Wiqo_$version.msixbundle"
& $makeappx.FullName bundle /d $stage /p $bundle /o
if ($LASTEXITCODE -ne 0) { throw 'makeappx could not build the bundle.' }
Remove-Item $stage -Recurse -Force
Write-Host "Built $bundle"
