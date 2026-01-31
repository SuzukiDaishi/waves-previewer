param(
    [string]$IssPath = "installer\NeoWaves.iss",
    [string]$IsccPath = "",
    [string]$OutputDir = "",
    [switch]$Quiet
)

$ErrorActionPreference = "Stop"

function Resolve-Iscc {
    param([string]$Override)
    if ($Override -and (Test-Path $Override)) {
        return (Resolve-Path $Override).Path
    }
    $candidates = @(
        "$env:ProgramFiles\Inno Setup 6\ISCC.exe",
        "$env:ProgramFiles(x86)\Inno Setup 6\ISCC.exe",
        "$env:ProgramFiles\Inno Setup 5\ISCC.exe",
        "$env:ProgramFiles(x86)\Inno Setup 5\ISCC.exe",
        "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe",
        "$env:LOCALAPPDATA\Programs\Inno Setup 5\ISCC.exe",
        "$env:LOCALAPPDATA\Microsoft\WinGet\Links\ISCC.exe",
        "$env:ProgramData\chocolatey\bin\ISCC.exe"
    )
    foreach ($c in $candidates) {
        if ($c -and (Test-Path $c)) { return $c }
    }
    $regKeys = @(
        "HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\Inno Setup 6_is1",
        "HKLM:\SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\Inno Setup 6_is1",
        "HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\Inno Setup 5_is1",
        "HKLM:\SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\Inno Setup 5_is1"
    )
    foreach ($key in $regKeys) {
        if (Test-Path $key) {
            try {
                $loc = (Get-ItemProperty -Path $key -Name "InstallLocation" -ErrorAction Stop).InstallLocation
                if ($loc) {
                    $cand = Join-Path $loc "ISCC.exe"
                    if (Test-Path $cand) { return $cand }
                }
                $icon = (Get-ItemProperty -Path $key -Name "DisplayIcon" -ErrorAction SilentlyContinue).DisplayIcon
                if ($icon) {
                    $iconPath = $icon -split "," | Select-Object -First 1
                    if (Test-Path $iconPath) { return $iconPath }
                }
            } catch {}
        }
    }
    $cmd = Get-Command ISCC.exe -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }
    throw "ISCC.exe not found. Install Inno Setup or pass -IsccPath."
}

$issFull = Resolve-Path $IssPath
$root = Split-Path -Parent $issFull
$workdir = $root
$iscc = Resolve-Iscc $IsccPath

$args = @()
if ($OutputDir) {
    $outFull = Resolve-Path -Path $OutputDir -ErrorAction SilentlyContinue
    if (-not $outFull) {
        New-Item -ItemType Directory -Force -Path $OutputDir | Out-Null
        $outFull = Resolve-Path $OutputDir
    }
    $args += "/O$outFull"
}
if ($Quiet) { $args += "/Q" }
$args += $issFull

Write-Host "Using ISCC: $iscc"
Write-Host "Building: $issFull"

$proc = Start-Process -FilePath $iscc -ArgumentList $args -WorkingDirectory $workdir -NoNewWindow -PassThru -Wait
if ($proc.ExitCode -ne 0) {
    throw "ISCC failed with exit code $($proc.ExitCode)"
}

Write-Host "Done."
