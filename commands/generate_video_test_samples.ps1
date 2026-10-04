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
# Otherwise follow a WinGet link to the real binary: run from its own bin
# directory, the shared build finds its DLLs.
$ffmpegLink = Get-Item $ffmpeg
if ($ffmpegLink.LinkType -eq "SymbolicLink") {
    $ffmpeg = $ffmpegLink.Target | Select-Object -First 1
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

# ProRes: four 64x48 frames at 24 fps. The left half is red, green, blue,
# white on frames 0-3 so a test can tell which frame it got; the right half is
# fully transparent in the 4444 file (and black in the opaque 422 one). The
# colour description is written as BT.709 so the frame header declares it.
$proresSource = "color=c=black:s=64x48:r=24,format=rgba," +
    "geq=r='if(lt(X,32),255*(eq(N,0)+eq(N,3)),0)'" +
    ":g='if(lt(X,32),255*(eq(N,1)+eq(N,3)),0)'" +
    ":b='if(lt(X,32),255*(eq(N,2)+eq(N,3)),0)'" +
    ":a='if(lt(X,32),255,0)'"
$proresFixtures = @(
    @{ Name = "prores_4444_alpha_64x48.mov"; Profile = "4444"; PixFmt = "yuva444p10le" },
    @{ Name = "prores_422hq_64x48.mov"; Profile = "hq"; PixFmt = "yuv422p10le" }
)
$proresOutputs = @()
foreach ($fixture in $proresFixtures) {
    $proresPath = Join-Path $resolvedOutput $fixture.Name
    & $ffmpeg @(
        "-hide_banner", "-loglevel", "error", "-y",
        "-f", "lavfi", "-i", $proresSource, "-frames:v", "4",
        "-vf", "scale=out_color_matrix=bt709:out_range=tv,format=$($fixture.PixFmt)",
        "-c:v", "prores_ks", "-profile:v", $fixture.Profile,
        "-colorspace", "bt709", "-color_primaries", "bt709",
        "-color_trc", "bt709", "-color_range", "tv",
        "-bitexact", $proresPath
    )
    if ($LASTEXITCODE -ne 0) {
        throw "ffmpeg failed while creating $proresPath"
    }
    $proresOutputs += $proresPath
}

# MPEG-TS: AVCHD-shaped .mts / .m2ts files. `-mpegts_m2ts_mode 1` writes the
# 192-byte packets (a 4-byte arrival timestamp ahead of each 188-byte TS
# packet) that camcorders and Blu-ray use. Small pictures keep them light;
# what they test is the container, the audio codecs and the timing.
$mpegTsOutputs = @()
function Invoke-MpegTsFixture([string]$Name, [string[]]$Arguments) {
    $path = Join-Path $resolvedOutput $Name
    & $ffmpeg @("-hide_banner", "-loglevel", "error", "-y") @Arguments @(
        "-f", "mpegts", "-mpegts_m2ts_mode", "1", $path
    )
    if ($LASTEXITCODE -ne 0) {
        throw "ffmpeg failed while creating $path"
    }
    $script:mpegTsOutputs += $path
}

# The sync picture and tick of the MP4 above, with AC-3 audio that starts
# 0.3 s after the picture (atrim keeps the original timestamps). The ticks
# still fall on whole seconds of the movie, so audio and picture agree only
# if the app lines the two streams up by their PTS.
Invoke-MpegTsFixture "mts_sync_ac3_6s.mts" @(
    "-f", "lavfi", "-i", $videoInput,
    "-f", "lavfi", "-i", "sine=frequency=880:sample_rate=48000:duration=6",
    "-vf", "$videoFilter,scale=640:360",
    "-af", "atrim=start=0.3,volume='if(lt(mod(t,1),0.08),0.75,0)':eval=frame",
    "-c:v", "libx264", "-preset", "veryfast", "-crf", "30",
    "-pix_fmt", "yuv420p", "-g", "15", "-keyint_min", "15", "-sc_threshold", "0",
    "-c:a", "ac3", "-b:a", "192k", "-ac", "2"
)

# 5.1 with a different tone in every channel (L 300, R 450, C 600, LFE 60,
# Ls 750, Rs 900 Hz), once as AC-3 and once as 24-bit Blu-ray LPCM, so a test
# can tell which speaker each decoded channel came from. The LPCM one is half
# a second: uncompressed 5.1 is 860 KB a second.
function Get-SurroundTones([string]$Secs) {
    "sine=f=300:r=48000:d=$Secs[l];sine=f=450:r=48000:d=$Secs[r];" +
    "sine=f=600:r=48000:d=$Secs[c];sine=f=60:r=48000:d=$Secs[lfe];" +
    "sine=f=750:r=48000:d=$Secs[ls];sine=f=900:r=48000:d=$Secs[rs];" +
    "[l][r][c][lfe][ls][rs]join=inputs=6:channel_layout=5.1(side)" +
    # Explicit: left to guess, join gives the first mono input (which it
    # calls FC) to the centre and shifts the rest along.
    ":map=0.0-FL|1.0-FR|2.0-FC|3.0-LFE|4.0-SL|5.0-SR[a]"
}
$tinyPicture = "color=c=0x334155:size=160x90:rate=30:duration=2"
Invoke-MpegTsFixture "m2ts_ac3_51.m2ts" @(
    "-f", "lavfi", "-i", $tinyPicture,
    "-filter_complex", (Get-SurroundTones "2"), "-map", "0:v", "-map", "[a]",
    "-c:v", "libx264", "-preset", "veryfast", "-crf", "35", "-pix_fmt", "yuv420p",
    "-c:a", "ac3", "-b:a", "384k"
)
Invoke-MpegTsFixture "m2ts_lpcm_51.m2ts" @(
    "-f", "lavfi", "-i", "color=c=0x334155:size=160x90:rate=30:duration=0.5",
    "-filter_complex", (Get-SurroundTones "0.5"), "-map", "0:v", "-map", "[a]",
    "-c:v", "libx264", "-preset", "veryfast", "-crf", "35", "-pix_fmt", "yuv420p",
    "-c:a", "pcm_bluray", "-sample_fmt", "s32"
)

# 1080i the way AVCHD records it in its HX/LP modes: 1440x1080 stored, 4:3
# pixels, top field first, with motion so an un-deinterlaced picture combs.
Invoke-MpegTsFixture "mts_1440x1080i.mts" @(
    "-f", "lavfi", "-i", "testsrc2=size=1920x1080:rate=60:duration=1",
    "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000:duration=1",
    "-vf", "scale=1440:1080,setsar=4/3,tinterlace=interleave_top,fieldorder=tff",
    "-c:v", "libx264", "-preset", "veryfast", "-crf", "32", "-pix_fmt", "yuv420p",
    "-flags", "+ildct+ilme", "-x264-params", "tff=1",
    "-c:a", "ac3", "-b:a", "192k", "-ac", "2"
)

# E-AC-3 (Dolby Digital Plus), which the app names but does not decode: the
# row must say E-AC-3 UNSUPPORTED and play its picture on a silent timeline.
Invoke-MpegTsFixture "m2ts_eac3_unsupported.m2ts" @(
    "-f", "lavfi", "-i", "color=c=0x334155:size=160x90:rate=30:duration=0.5",
    "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000:duration=0.5",
    "-c:v", "libx264", "-preset", "veryfast", "-crf", "35", "-pix_fmt", "yuv420p",
    "-c:a", "eac3", "-b:a", "96k", "-ac", "2"
)

Invoke-MpegTsFixture "mts_no_audio.mts" @(
    "-f", "lavfi", "-i", $tinyPicture,
    "-c:v", "libx264", "-preset", "veryfast", "-crf", "35", "-pix_fmt", "yuv420p",
    "-an"
)

Write-Host "Created deterministic video fixtures:"
Write-Host "  $withAudio"
Write-Host "  $withoutAudio"
foreach ($proresPath in $proresOutputs) {
    Write-Host "  $proresPath"
}
foreach ($mpegTsPath in $mpegTsOutputs) {
    Write-Host "  $mpegTsPath"
}
