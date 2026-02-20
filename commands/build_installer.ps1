param(
    [string]$IssPath = "installer\NeoWaves.iss",
    [string]$IsccPath = "",
    [string]$OutputDir = "",
    [string]$AppVersion = "",
    [string]$BuildId = "",
    [switch]$SkipCargoBuild,
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

$cargoToml = $null
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
if (-not $cargoToml) {
    $cargoToml = Find-CargoToml $root
    if (-not $cargoToml) {
        throw "Cargo.toml not found (run from repo root)."
    }
}
$repoRoot = Split-Path -Parent $cargoToml

function New-BuildId {
    return (Get-Date).ToString("yyyyMMdd_HHmmss")
}

function Sync-RuntimeDlls {
    param([string]$RepoRoot)
    $releaseDir = Join-Path $RepoRoot "target\release"
    $depsDir = Join-Path $releaseDir "deps"
    if (-not (Test-Path $releaseDir)) {
        return
    }
    $patterns = @(
        "onnxruntime*.dll",
        "onnxruntime_providers*.dll",
        "dnnl*.dll",
        "mklml*.dll",
        "onig*.dll"
    )
    $copied = New-Object System.Collections.Generic.HashSet[string]
    foreach ($pat in $patterns) {
        $sources = @()
        if (Test-Path $depsDir) {
            $sources += Get-ChildItem -Path $depsDir -Filter $pat -File -ErrorAction SilentlyContinue
        }
        $sources += Get-ChildItem -Path $releaseDir -Filter $pat -File -ErrorAction SilentlyContinue
        foreach ($src in $sources) {
            $name = $src.Name.ToLowerInvariant()
            if ($copied.Contains($name)) { continue }
            $dst = Join-Path $releaseDir $src.Name
            if ($src.FullName -ne $dst) {
                Copy-Item -Path $src.FullName -Destination $dst -Force
            }
            [void]$copied.Add($name)
        }
    }
    if ($copied.Count -gt 0) {
        Write-Host ("Runtime DLLs prepared: " + (($copied | Sort-Object) -join ", "))
    } else {
        Write-Host "Runtime DLLs prepared: none found"
    }
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

function Invoke-IsccCommand {
    param(
        [string]$ExePath,
        [string[]]$ExeArgs
    )
    $stdoutPath = [System.IO.Path]::GetTempFileName()
    $stderrPath = [System.IO.Path]::GetTempFileName()
    try {
        $proc = Start-Process `
            -FilePath $ExePath `
            -ArgumentList $ExeArgs `
            -Wait `
            -PassThru `
            -NoNewWindow `
            -RedirectStandardOutput $stdoutPath `
            -RedirectStandardError $stderrPath

        $out = @()
        if (Test-Path $stdoutPath) {
            $out += Get-Content -Path $stdoutPath -ErrorAction SilentlyContinue
        }
        if (Test-Path $stderrPath) {
            $out += Get-Content -Path $stderrPath -ErrorAction SilentlyContinue
        }
        $exitCode = if ($null -ne $proc) { [int]$proc.ExitCode } else { 1 }
        $text = ($out -join "`n")
        [pscustomobject]@{
            Output = $out
            ExitCode = $exitCode
            Text = $text
        }
    } finally {
        Remove-Item -Path $stdoutPath -ErrorAction SilentlyContinue
        Remove-Item -Path $stderrPath -ErrorAction SilentlyContinue
    }
}

if (-not $BuildId) {
    $BuildId = New-BuildId
}
if (-not $OutputDir) {
    $OutputDir = Join-Path $root ("out\\installer_" + $BuildId)
}

if (-not $SkipCargoBuild) {
    Write-Host "Building release binaries (cargo build --release --bins)..."
    Push-Location $repoRoot
    try {
        & cargo build --release --bins
        if ($LASTEXITCODE -ne 0) {
            throw "cargo build failed with exit code $LASTEXITCODE"
        }
    } finally {
        Pop-Location
    }
    Sync-RuntimeDlls -RepoRoot $repoRoot
}

$args = Build-Args -OutDir $OutputDir -Ver $version -Id $BuildId

Write-Host "Using ISCC: $iscc"
Write-Host "Building: $issFull"
Write-Host "AppVersion: $version"
Write-Host "BuildId: $BuildId"
Write-Host "OutputDir: $OutputDir"

$attempts = 0
$maxAttempts = 5
$usedTempOutputFallback = $false
while ($true) {
    $attempts++
    $run = Invoke-IsccCommand -ExePath $iscc -ExeArgs $args
    $output = $run.Output
    $code = $run.ExitCode
    $text = $run.Text
    $resourceUpdateError = ($text -match "Resource update error") -or ($text -match "EndUpdateResource failed")
    if ($code -eq 0) {
        if ($output) { $output | Write-Host }
        break
    }
    if ($output) { $output | Write-Host }
    if (-not $resourceUpdateError -or $attempts -ge $maxAttempts) {
        if ($resourceUpdateError) {
            throw "ISCC failed after $attempts attempts due to resource update error (110). Try excluding installer output/temp folders from antivirus software."
        }
        throw "ISCC failed with exit code $code"
    }
    if (-not $usedTempOutputFallback -and $attempts -ge 2) {
        $usedTempOutputFallback = $true
        $OutputDir = Join-Path $env:TEMP ("neowaves_installer_" + $BuildId)
        Write-Host "ISCC resource update failed (110). Switching OutputDir to temp path: $OutputDir"
    } else {
        Write-Host "ISCC resource update failed (110). Retrying with new BuildId..."
    }
    $delayMs = [Math]::Min(5000, 500 * [Math]::Pow(2, $attempts - 1))
    Start-Sleep -Milliseconds ([int]$delayMs)
    $BuildId = New-BuildId
    if (-not $usedTempOutputFallback) {
        $OutputDir = Join-Path $root ("out\\installer_" + $BuildId)
    } else {
        $OutputDir = Join-Path $env:TEMP ("neowaves_installer_" + $BuildId)
    }
    $args = Build-Args -OutDir $OutputDir -Ver $version -Id $BuildId
    Write-Host "Retrying ISCC ($($attempts + 1)/$maxAttempts)..."
}

if ($usedTempOutputFallback) {
    Write-Host "Installer output fallback used. Final OutputDir: $OutputDir"
}

Write-Host "Done."
