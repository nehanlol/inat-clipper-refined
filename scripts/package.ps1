param(
    [string]$Target = "x86_64-pc-windows-msvc"
)

$ErrorActionPreference = "Stop"
$ProjectRoot = Split-Path -Parent $PSScriptRoot
$ToolsDirectory = Join-Path $ProjectRoot "tools"
$DistributionDirectory = Join-Path $ProjectRoot "dist"
$BuildDirectory = Join-Path $ProjectRoot "target\$Target\release"

# These are built into inat-gui.exe rather than shipped beside it, which is what
# makes the window a single file that runs on a machine with nothing installed.
# ffprobe is deliberately not among them: yt-dlp does everything INAT asks of it
# with ffmpeg alone, and it would have doubled the download for nothing.
$BundledTools = @("yt-dlp.exe", "ffmpeg.exe")
foreach ($Tool in $BundledTools) {
    $ToolPath = Join-Path $ToolsDirectory $Tool
    if (-not (Test-Path -LiteralPath $ToolPath -PathType Leaf)) {
        throw "Missing $ToolPath. The backend binaries are compiled into the window, so they must be here before building."
    }
}

cargo build --release --target $Target
if ($LASTEXITCODE -ne 0) {
    throw "Cargo build failed."
}

# inat-gui.exe is the product: one file, nothing beside it. inat.exe is the
# terminal front end, which stays small by leaving the backends to the window -
# it finds whatever the window unpacked.
$Executables = @("inat-gui.exe", "inat.exe")
foreach ($Executable in $Executables) {
    $Path = Join-Path $BuildDirectory $Executable
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "Missing $Path after the build."
    }
}

New-Item -ItemType Directory -Force -Path $DistributionDirectory | Out-Null
foreach ($Executable in $Executables) {
    Copy-Item -LiteralPath (Join-Path $BuildDirectory $Executable) -Destination $DistributionDirectory -Force
}

$Size = (Get-Item (Join-Path $DistributionDirectory "inat-gui.exe")).Length / 1MB
Write-Host ("Built {0}" -f $DistributionDirectory)
Write-Host ("  inat-gui.exe  {0:N0} MB   the app, self-contained" -f $Size)
Write-Host  "  inat.exe                  optional terminal front end"
