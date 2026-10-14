<#
.SYNOPSIS
    Builds the production release binary and compiles the standalone Inno Setup installer for INAT.
#>
param(
    [string]$Target = "x86_64-pc-windows-gnu"
)

$ErrorActionPreference = "Stop"
$ProjectRoot = Split-Path -Parent $PSScriptRoot
Set-Location $ProjectRoot

# Ensure MinGW GCC is in PATH for linking
$MinGwBin = "$env:LOCALAPPDATA\Microsoft\WinGet\Packages\BrechtSanders.WinLibs.POSIX.UCRT_Microsoft.Winget.Source_8wekyb3d8bbwe\mingw64\bin"
if (Test-Path $MinGwBin) {
    $env:Path = "$MinGwBin;$env:Path"
}

Write-Host "==> Compiling INAT release binary..." -ForegroundColor Cyan
& "$env:USERPROFILE\.cargo\bin\cargo.exe" build --release --bin inat-gui

New-Item -ItemType Directory -Force -Path "dist" | Out-Null
Copy-Item "target\release\inat-gui.exe" -Destination "dist\inat-gui.exe" -Force

Write-Host "==> Compiling Inno Setup installer..." -ForegroundColor Cyan
$Iscc = "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe"
if (-not (Test-Path $Iscc)) {
    $Iscc = "ISCC.exe"
}
& $Iscc "installer\inat_setup.iss"

Write-Host ""
Write-Host "========================================================" -ForegroundColor Green
Write-Host "  Installer generated successfully!" -ForegroundColor Green
Write-Host "  Location: dist\INAT_Setup_v1.0.0.exe" -ForegroundColor Green
Write-Host "========================================================" -ForegroundColor Green
