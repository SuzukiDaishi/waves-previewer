param(
    [string]$Mp3Path = "debug/long_load_test.mp3",
    [int]$DurationSecs = 180,
    [int]$DelayFrames = 240
)

$ErrorActionPreference = "Stop"

$repo = Split-Path -Parent $PSScriptRoot
Set-Location $repo

$mp3 = [System.IO.Path]::GetFullPath((Join-Path $repo $Mp3Path))
$debugDir = Join-Path $repo "debug"
$logPath = Join-Path $debugDir "measure_editor_mp3_load.log"
$summaryPath = Join-Path $debugDir "measure_editor_mp3_load_summary.txt"
$shotPath = Join-Path $debugDir "measure_editor_mp3_load.png"

if (-not (Test-Path $mp3)) {
    cargo run --bin debug_generate_long_mp3 -- "$mp3" "$DurationSecs"
}

if (Test-Path $logPath) { Remove-Item $logPath -Force }
if (Test-Path $summaryPath) { Remove-Item $summaryPath -Force }
if (Test-Path $shotPath) { Remove-Item $shotPath -Force }

cargo run -- `
    --debug `
    --debug-log $logPath `
    --open-file $mp3 `
    --open-first `
    --debug-summary $summaryPath `
    --debug-summary-delay $DelayFrames `
    --screenshot $shotPath `
    --screenshot-delay $DelayFrames `
    --exit-after-screenshot

Write-Host ""
Write-Host "Summary: $summaryPath"
if (Test-Path $summaryPath) {
    Get-Content $summaryPath | Select-String "editor_open_to_|frame_ms|processing:|tabs:|selected:"
}

Write-Host ""
Write-Host "Log: $logPath"
if (Test-Path $logPath) {
    Get-Content $logPath | Select-String "editor open |editor decode spawn"
}
