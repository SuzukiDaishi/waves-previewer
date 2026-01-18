# Generate a simple sine-wave WAV file for spectrogram validation.

[CmdletBinding()]
param(
    [double]$Frequency = 440.0,
    [double]$DurationSeconds = 2.0,
    [int]$SampleRate = 48000,
    [int]$Channels = 1,
    [string]$OutPath = (Join-Path $PSScriptRoot "..\\debug\\sine_${Frequency}Hz.wav")
)

$ErrorActionPreference = "Stop"

if ($Frequency -le 0) { throw "Frequency must be > 0." }
if ($DurationSeconds -le 0) { throw "DurationSeconds must be > 0." }
if ($SampleRate -le 0) { throw "SampleRate must be > 0." }
if ($Channels -lt 1) { throw "Channels must be >= 1." }

$samples = [int][Math]::Round($SampleRate * $DurationSeconds)
$bytesPerSample = 2 # 16-bit PCM
$blockAlign = $Channels * $bytesPerSample
$byteRate = $SampleRate * $blockAlign
$dataSize = $samples * $blockAlign

$outDir = Split-Path -Parent $OutPath
if (-not (Test-Path $outDir)) {
    New-Item -ItemType Directory -Force -Path $outDir | Out-Null
}

$fs = [System.IO.File]::Open($OutPath, [System.IO.FileMode]::Create, [System.IO.FileAccess]::Write, [System.IO.FileShare]::None)
$bw = [System.IO.BinaryWriter]::new($fs)

try {
    # RIFF header
    $bw.Write([System.Text.Encoding]::ASCII.GetBytes("RIFF"))
    $bw.Write([int] (36 + $dataSize))
    $bw.Write([System.Text.Encoding]::ASCII.GetBytes("WAVE"))
    # fmt chunk
    $bw.Write([System.Text.Encoding]::ASCII.GetBytes("fmt "))
    $bw.Write([int]16) # PCM
$bw.Write([int16]1) # PCM format
$bw.Write([int16]$Channels)
    $bw.Write([int]$SampleRate)
    $bw.Write([int]$byteRate)
$bw.Write([int16]$blockAlign)
$bw.Write([int16]16) # bits per sample
    # data chunk
    $bw.Write([System.Text.Encoding]::ASCII.GetBytes("data"))
    $bw.Write([int]$dataSize)

    $twoPiF = 2.0 * [Math]::PI * $Frequency
    for ($i = 0; $i -lt $samples; $i++) {
        $t = $i / [double]$SampleRate
        $v = [Math]::Sin($twoPiF * $t)
        $sample = [int16][Math]::Round($v * 32767.0)
        for ($c = 0; $c -lt $Channels; $c++) {
            $bw.Write($sample)
        }
    }
} finally {
    $bw.Dispose()
    $fs.Dispose()
}

Write-Host "Wrote: $OutPath"
