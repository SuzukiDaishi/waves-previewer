param(
    [string]$OutputDir = (Join-Path $PSScriptRoot "..\test_samples\video")
)

$ErrorActionPreference = "Stop"

# Prefer the self-contained WinGet build. A WinGet link can point at the
# shared build without putting its DLL directory on PATH, which exits with
# STATUS_DLL_NOT_FOUND before ffmpeg can print an error.
$staticPattern = Join-Path $env:LOCALAPPDATA "Microsoft\WinGet\Packages\Gyan.FFmpeg_Microsoft.Winget.Source_*\ffmpeg-*-full_build\bin\ffmpeg.exe"
$staticFfmpeg = Get-ChildItem $staticPattern -ErrorAction SilentlyContinue | Select-Object -First 1
$ffmpeg = if ($staticFfmpeg) {
    $staticFfmpeg.FullName
} else {
    (Get-Command ffmpeg -ErrorAction Stop).Source
}
$resolvedOutput = [System.IO.Path]::GetFullPath($OutputDir)
New-Item -ItemType Directory -Force -Path $resolvedOutput | Out-Null

$videoInput = "color=c=0x111827:size=1920x1080:rate=30:duration=6"
$videoFilter = "drawbox=x=0:y=162:w=480:h=756:color=red@0.85:t=fill," +
    "drawbox=x=480:y=162:w=480:h=756:color=green@0.85:t=fill," +
    "drawbox=x=960:y=162:w=480:h=756:color=blue@0.85:t=fill," +
    "drawbox=x=1440:y=162:w=480:h=756:color=magenta@0.85:t=fill," +
    "drawgrid=width=240:height=135:thickness=3:color=white@0.24," +
    "drawbox=x=0:y=810:w=240:h=162:color=yellow@0.95:t=fill:enable='between(t\,0\,1)'," +
    "drawbox=x=288:y=810:w=240:h=162:color=yellow@0.95:t=fill:enable='between(t\,1\,2)'," +
    "drawbox=x=576:y=810:w=240:h=162:color=yellow@0.95:t=fill:enable='between(t\,2\,3)'," +
    "drawbox=x=864:y=810:w=240:h=162:color=yellow@0.95:t=fill:enable='between(t\,3\,4)'," +
    "drawbox=x=1152:y=810:w=240:h=162:color=yellow@0.95:t=fill:enable='between(t\,4\,5)'," +
    "drawbox=x=1440:y=810:w=240:h=162:color=yellow@0.95:t=fill:enable='between(t\,5\,6)'," +
    "drawbox=x=0:y=0:w=iw:h=162:color=black@0.70:t=fill," +
    "drawtext=text='SYNC %{pts\:hms}':x=54:y=36:fontsize=84:fontcolor=white"
$videoArgs = @(
    "-hide_banner", "-loglevel", "error", "-y",
    "-f", "lavfi", "-i", $videoInput,
    "-vf", $videoFilter,
    "-c:v", "libx264", "-preset", "veryfast", "-crf", "24",
    "-pix_fmt", "yuv420p", "-g", "30", "-keyint_min", "30",
    "-sc_threshold", "0", "-movflags", "+faststart"
)

$withAudio = Join-Path $resolvedOutput "video_sync_6s_30fps.mp4"
& $ffmpeg @(
    "-hide_banner", "-loglevel", "error", "-y",
    "-f", "lavfi", "-i", $videoInput,
    "-f", "lavfi", "-i", "sine=frequency=880:sample_rate=48000:duration=6",
    "-vf", $videoFilter,
    "-af", "volume='if(lt(mod(t,1),0.08),0.75,0)':eval=frame",
    "-c:v", "libx264", "-preset", "veryfast", "-crf", "24",
    "-pix_fmt", "yuv420p", "-g", "30", "-keyint_min", "30",
    "-sc_threshold", "0", "-c:a", "aac", "-b:a", "96k",
    "-shortest", "-movflags", "+faststart", $withAudio
)
if ($LASTEXITCODE -ne 0) {
    throw "ffmpeg failed while creating $withAudio"
}

$withoutAudio = Join-Path $resolvedOutput "video_no_audio_6s_30fps.mp4"
& $ffmpeg @videoArgs "-an" $withoutAudio
if ($LASTEXITCODE -ne 0) {
    throw "ffmpeg failed while creating $withoutAudio"
}

Write-Host "Created deterministic video fixtures:"
Write-Host "  $withAudio"
Write-Host "  $withoutAudio"
