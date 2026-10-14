param([string]$TargetExe)
if (-not (Test-Path $TargetExe)) { exit 0 }
try {
    $shell = New-Object -ComObject Shell.Application
    $folder = $shell.Namespace((Split-Path $TargetExe))
    $item = $folder.ParseName((Split-Path $TargetExe -Leaf))
    if ($item) {
        foreach ($verb in $item.Verbs()) {
            if ($verb.Name -replace '&','' -match 'pin to taskbar') {
                $verb.DoIt()
                break
            }
        }
    }
} catch {
    # Non-critical: taskbar pin is subject to Windows user policy
}
