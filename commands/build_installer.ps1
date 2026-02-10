param(
    [string]$IssPath = "installer\NeoWaves.iss",
    [string]$IsccPath = "",
    [string]$OutputDir = "",
    [string]$AppVersion = "",
    [string]$BuildId = "",
    [switch]$NoAutoVersion,
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

function Find-CargoToml {
    param([string]$StartDir)
    $dir = (Resolve-Path $StartDir).Path
    while ($dir) {
        $cand = Join-Path $dir "Cargo.toml"
        if (Test-Path $cand) { return $cand }
        $parent = Split-Path -Parent $dir
        if ($parent -eq $dir) { break }
        $dir = $parent
    }
    return $null
}

function Get-AppVersionFromCargo {
    param([string]$CargoTomlPath)
    $lines = Get-Content -Path $CargoTomlPath
    $inPackage = $false
    foreach ($line in $lines) {
        if ($line -match '^\s*\[(.+)\]\s*$') {
            $section = $Matches[1]
            $inPackage = $section -eq 'package'
            continue
        }
        if ($inPackage -and $line -match '^\s*version\s*=\s*"([^"]+)"') {
            return $Matches[1]
        }
    }
    throw "Could not read version from $CargoTomlPath"
}

function Get-TodayVersion {
    $today = Get-Date
    $datePart = $today.ToString("yyyyMMdd")
    return "0.$datePart.0"
}

function Update-CargoVersionToToday {
    param([string]$CargoTomlPath)
    $current = Get-AppVersionFromCargo $CargoTomlPath
    $today = (Get-Date).ToString("yyyyMMdd")
    $next = "0.$today.0"
    if ($current -match '^0\.(\d{8})\.(\d+)$') {
        $curDate = $Matches[1]
        $curN = [int]$Matches[2]
        if ($curDate -eq $today) {
            $next = "0.$today.$($curN + 1)"
        }
    }
    $lines = Get-Content -Path $CargoTomlPath
    $inPackage = $false
    $updated = $false
    for ($i = 0; $i -lt $lines.Count; $i++) {
        $line = $lines[$i]
        if ($line -match '^\s*\[(.+)\]\s*$') {
            $section = $Matches[1]
            $inPackage = $section -eq 'package'
            continue
        }
        if ($inPackage -and $line -match '^\s*version\s*=') {
            $lines[$i] = "version = `"$next`""
            $updated = $true
            break
        }
    }
    if (-not $updated) {
        throw "Could not update version in $CargoTomlPath"
    }
    Set-Content -Path $CargoTomlPath -Value $lines -Encoding UTF8
    return $next
}

$issFull = Resolve-Path $IssPath
$root = Split-Path -Parent $issFull
$workdir = $root
$iscc = Resolve-Iscc $IsccPath

$version = $AppVersion
if (-not $version) {
    $cargoToml = Find-CargoToml $root
    if (-not $cargoToml) {
        throw "Cargo.toml not found (set -AppVersion or run from repo)"
    }
    if (-not $NoAutoVersion) {
        $version = Update-CargoVersionToToday $cargoToml
    } else {
        $version = Get-AppVersionFromCargo $cargoToml
    }
}

function New-BuildId {
    return (Get-Date).ToString("yyyyMMdd_HHmmss")
}

function Build-Args {
    param(
        [string]$OutDir,
        [string]$Ver,
        [string]$Id
    )
    $localArgs = @()
    if ($OutDir) {
        $outFull = Resolve-Path -Path $OutDir -ErrorAction SilentlyContinue
        if (-not $outFull) {
            New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
            $outFull = Resolve-Path $OutDir
        }
        $localArgs += "/O$outFull"
    }
    if ($Quiet) { $localArgs += "/Q" }
    if ($Id) { $localArgs += "/DMyAppBuildId=$Id" }
    $localArgs += "/DMyAppVersion=$Ver"
    $localArgs += $issFull
    return $localArgs
}

if (-not $BuildId) {
    $BuildId = New-BuildId
}
if (-not $OutputDir) {
    $OutputDir = Join-Path $root ("out\\installer_" + $BuildId)
}

$args = Build-Args -OutDir $OutputDir -Ver $version -Id $BuildId

Write-Host "Using ISCC: $iscc"
Write-Host "Building: $issFull"
Write-Host "AppVersion: $version"
Write-Host "BuildId: $BuildId"
Write-Host "OutputDir: $OutputDir"

$attempts = 0
$maxAttempts = 3
while ($true) {
    $attempts++
    $output = & $iscc @args 2>&1
    $code = $LASTEXITCODE
    if ($code -eq 0) {
        if ($output) { $output | Write-Host }
        break
    }
    if ($code -ne 2 -or $attempts -ge $maxAttempts) {
        if ($output) { $output | Write-Host }
        throw "ISCC failed with exit code $code"
    }
    Write-Host "ISCC resource update failed (110). Retrying with new BuildId..."
    Start-Sleep -Milliseconds 800
    $BuildId = New-BuildId
    $OutputDir = Join-Path $root ("out\\installer_" + $BuildId)
    $args = Build-Args -OutDir $OutputDir -Ver $version -Id $BuildId
}

Write-Host "Done."
