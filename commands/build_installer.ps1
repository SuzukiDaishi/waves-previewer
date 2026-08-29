param(
    [string]$NsiPath = "installer\NeoWaves.nsi",
    [string]$MakensisPath = "",
    [string]$OutputDir = "",
    [string]$AppVersion = "",
    [string]$BuildId = "",
    [ValidateSet("", "admin", "lowest", "poweruser")]
    [string]$PrivilegesRequired = "",
    [switch]$SkipCargoBuild,
    [switch]$NoAutoVersion,
    [switch]$Quiet
)

$ErrorActionPreference = "Stop"

function Resolve-Makensis {
    param([string]$Override)
    if ($Override -and (Test-Path $Override)) {
        return (Resolve-Path $Override).Path
    }
    $candidates = @(
        "$env:ProgramFiles\NSIS\makensis.exe",
        "${env:ProgramFiles(x86)}\NSIS\makensis.exe",
        "$env:LOCALAPPDATA\Programs\NSIS\makensis.exe",
        "$env:ProgramData\chocolatey\bin\makensis.exe"
    )
    foreach ($c in $candidates) {
        if ($c -and (Test-Path $c)) { return $c }
    }
    $regKeys = @(
        "HKLM:\SOFTWARE\NSIS",
        "HKLM:\SOFTWARE\WOW6432Node\NSIS"
    )
    foreach ($key in $regKeys) {
        if (Test-Path $key -ErrorAction SilentlyContinue) {
            try {
                # NSIS records its install directory as the key's default value,
                # which only GetValue("") reads reliably.
                $loc = (Get-Item -Path $key -ErrorAction Stop).GetValue("")
                if ($loc) {
                    $cand = Join-Path $loc "makensis.exe"
                    if (Test-Path $cand) { return $cand }
                }
            } catch {}
        }
    }
    $cmd = Get-Command makensis.exe -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }
    $cmd = Get-Command makensis -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }
    throw "makensis.exe not found. Install NSIS (choco install nsis) or pass -MakensisPath."
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

$nsiFull = Resolve-Path $NsiPath
$root = Split-Path -Parent $nsiFull
$workdir = $root
$makensis = Resolve-Makensis $MakensisPath

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
        "libmp3lame.dll",
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
        [string]$OutFile,
        [string]$Ver,
        [string]$Id,
        [string]$SrcDir,
        [string]$RepoDir
    )
    # makensis has no /O switch: the output path is a define the script uses for
    # OutFile, so the caller decides the full path rather than just a directory.
    $localArgs = @()
    $localArgs += if ($Quiet) { "-V1" } else { "-V2" }
    $localArgs += "-DAPP_VERSION=$Ver"
    if ($Id) { $localArgs += "-DBUILD_ID=$Id" }
    $localArgs += "-DOUT_FILE=$OutFile"
    $localArgs += "-DSRC_DIR=$SrcDir"
    $localArgs += "-DREPO_DIR=$RepoDir"
    if ($PrivilegesRequired) { $localArgs += "-DPRIVILEGES=$PrivilegesRequired" }
    $localArgs += $nsiFull
    return $localArgs
}

function Invoke-Makensis {
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

function Show-UpdateSmokeGuidance {
    param(
        [string]$InstallerPath,
        [string]$Version
    )
    Write-Host ""
    Write-Host "Update smoke checklist:"
    Write-Host "1. Install an older NeoWaves build first, then close NeoWaves completely."
    Write-Host "2. Run the new installer over the existing install:"
    Write-Host "   $InstallerPath"
    Write-Host "3. Verify the installer reuses the previous install directory and closes running NeoWaves processes if needed."
    Write-Host "4. Launch NeoWaves and confirm:"
    Write-Host "   - Help/About or title bar reports version $Version"
    Write-Host "   - %APPDATA%\\NeoWaves\\prefs.txt is still present and settings are preserved"
    Write-Host "   - Existing file associations (.wav/.mp3/.m4a/.nwsess) still open NeoWaves if association task was enabled"
    Write-Host "   - Shell-open still appends to the list and opens the target file in Editor"
    Write-Host "5. Uninstall/reinstall only if you are explicitly testing clean-install behavior."
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

$installerBaseName = "NeoWaves-Setup-$version"
if ($BuildId) {
    $installerBaseName += "-$BuildId"
}
# release.yml locates the artifact by globbing installer\out for
# NeoWaves-Setup-*.exe, so this layout is part of the contract.
New-Item -ItemType Directory -Force -Path $OutputDir | Out-Null
$OutputDir = (Resolve-Path $OutputDir).Path
$installerPath = Join-Path $OutputDir ($installerBaseName + ".exe")

$srcDir = Join-Path (Join-Path $repoRoot "target") "release"
$makensisArgs = Build-Args -OutFile $installerPath -Ver $version -Id $BuildId -SrcDir $srcDir -RepoDir $repoRoot

Write-Host "Using makensis: $makensis"
Write-Host "Building: $nsiFull"
Write-Host "AppVersion: $version"
Write-Host "BuildId: $BuildId"
Write-Host "OutputDir: $OutputDir"

$run = Invoke-Makensis -ExePath $makensis -ExeArgs $makensisArgs
if ($run.Output) { $run.Output | Write-Host }
if ($run.ExitCode -ne 0) {
    throw "makensis failed with exit code $($run.ExitCode)"
}

Write-Host "Done."
if (Test-Path $installerPath) {
    Write-Host "InstallerPath: $installerPath"
} else {
    throw "makensis reported success but $installerPath is missing."
}
Show-UpdateSmokeGuidance -InstallerPath $installerPath -Version $version
