# commands/generate_srt_fast.ps1
# Parallel + Batch transcription to .srt using FFmpeg whisper filter (whisper.cpp)

[CmdletBinding()]
param(
  [Parameter(Mandatory = $true)]
  [string]$Root,

  [switch]$Recurse = $true,

  # Model path (auto-detect if empty)
  [string]$ModelPath = "",

  # Optional VAD model (silero). If empty, VAD is disabled.
  [string]$VadModelPath = "",
  [ValidateRange(0.0, 1.0)]
  [double]$VadThreshold = 0.5,
  [ValidateRange(0.0, 60.0)]
  [double]$VadMinSpeechSec = 0.1,
  [ValidateRange(0.0, 60.0)]
  [double]$VadMinSilenceSec = 0.5,

  [string]$Ffmpeg = "ffmpeg",
  [string]$Ffprobe = "ffprobe",

  [ValidateSet("auto","ja","en","zh","ko","fr","de","es","it","pt","ru")]
  [string]$Language = "auto",

  # Whisper queue in seconds (duration option, passed as "<N>s")
  [ValidateRange(1, 120)]
  [int]$QueueSeconds = 10,

  [switch]$NoGpu,
  [int]$GpuDevice = 0,

  # Parallel ffmpeg processes (0 => auto)
  [int]$Parallelism = 0,

  # Batch size (>1 enables -filter_complex batch mode)
  [ValidateRange(1, 256)]
  [int]$BatchSize = 1,

  # Optional: threads for filter graphs (0 => not specified)
  [int]$FilterThreads = 0,
  [int]$FilterComplexThreads = 0,

  [switch]$Force,

  [string[]]$Extensions = @("wav","mp3","m4a","aac","flac","ogg","opus","wma","mp4","mkv","mov","webm")
)

$ErrorActionPreference = "Stop"
[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)

function Quote-Arg([string]$s) {
  if ($null -eq $s) { return '""' }
  if ($s -match '[\s"]') { return '"' + ($s -replace '"','\"') + '"' }
  return $s
}
function Throw-IfEmpty([string]$value, [string]$what) {
  if ([string]::IsNullOrWhiteSpace($value)) { throw "Internal error: $what is empty." }
}
function Print-Bool([string]$label, [bool]$ok) {
  $mark = if ($ok) { "[OK]" } else { "[NG]" }
  Write-Host ("{0} {1}" -f $mark, $label)
}

function Run-Cmd([string]$exe, [string[]]$args) {
  $psi = New-Object System.Diagnostics.ProcessStartInfo
  $psi.FileName = $exe
  $psi.Arguments = ($args | ForEach-Object { Quote-Arg $_ }) -join " "
  $psi.UseShellExecute = $false
  $psi.RedirectStandardOutput = $true
  $psi.RedirectStandardError  = $true
  $psi.CreateNoWindow = $true
  $p = New-Object System.Diagnostics.Process
  $p.StartInfo = $psi
  $null = $p.Start()
  $stdout = $p.StandardOutput.ReadToEnd()
  $stderr = $p.StandardError.ReadToEnd()
  $p.WaitForExit()
  return [pscustomobject]@{ ExitCode=$p.ExitCode; Stdout=$stdout; Stderr=$stderr }
}

function Resolve-ExeCandidates([string]$nameOrPath) {
  if ($nameOrPath -and (Test-Path -LiteralPath $nameOrPath)) {
    return @((Resolve-Path -LiteralPath $nameOrPath).Path)
  }
  return @(Get-Command $nameOrPath -All -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Source)
}

function Has-WhisperFilter_PS([string]$ffmpegExe) {
  try {
    $txt = (& $ffmpegExe -hide_banner -filters 2>&1 | Out-String)
    return ($txt -match '(^|\s)whisper(\s|$)')
  } catch { return $false }
}

function Parse-Version([string]$text) {
  $m = [regex]::Match($text, 'ffmpeg version\s+([0-9]+)(?:\.([0-9]+))?(?:\.([0-9]+))?')
  if (-not $m.Success) { return $null }
  $maj = [int]$m.Groups[1].Value
  $min = if ($m.Groups[2].Success) { [int]$m.Groups[2].Value } else { 0 }
  $pat = if ($m.Groups[3].Success) { [int]$m.Groups[3].Value } else { 0 }
  return [pscustomobject]@{ Major=$maj; Minor=$min; Patch=$pat; Raw="$maj.$min.$pat" }
}
function Get-ConfigLine([string]$text) {
  foreach ($line in ($text -split "`r?`n")) {
    if ($line -match '^\s*configuration:\s*(.+)$') { return $Matches[1] }
  }
  return $null
}

function Select-BestFfmpeg([string]$ffmpegParam) {
  $cands = Resolve-ExeCandidates $ffmpegParam
  Print-Bool "ffmpeg found" ($cands.Count -gt 0)
  if ($cands.Count -eq 0) { throw "ffmpeg not found on PATH (or invalid -Ffmpeg path)." }

  $rows = @()
  foreach ($p in $cands) {
    $ver = Run-Cmd $p @("-hide_banner","-version")
    $txt = $ver.Stdout + "`n" + $ver.Stderr
    $v = Parse-Version $txt
    $cfg = Get-ConfigLine $txt
    $enableWhisper = $false
    if ($cfg) { $enableWhisper = ($cfg -match '--enable-whisper') }

    $usable = Has-WhisperFilter_PS $p
    $rows += [pscustomobject]@{
      Path = $p
      Version = if ($v) { $v.Raw } else { "" }
      Major = if ($v) { $v.Major } else { -1 }
      EnableWhisper = $enableWhisper
      WhisperUsable = $usable
    }
  }

  $best = $rows | Where-Object { $_.WhisperUsable } |
          Sort-Object -Property @{Expression='Major';Descending=$true} |
          Select-Object -First 1
  if (-not $best) {
    $best = $rows | Sort-Object -Property @{Expression='Major';Descending=$true} | Select-Object -First 1
  }
  return [pscustomobject]@{ Selected=$best; All=$rows }
}

function Select-Ffprobe([string]$ffprobeParam, [string]$selectedFfmpegPath) {
  if ($selectedFfmpegPath) {
    $dir = [System.IO.Path]::GetDirectoryName($selectedFfmpegPath)
    if (-not [string]::IsNullOrWhiteSpace($dir)) {
      $local = Join-Path $dir "ffprobe.exe"
      if (Test-Path -LiteralPath $local) { return (Resolve-Path -LiteralPath $local).Path }
    }
  }
  $cands = Resolve-ExeCandidates $ffprobeParam
  if ($cands.Count -gt 0) { return $cands[0] }
  return $null
}

function Preflight([string]$ffmpegParam, [string]$ffprobeParam) {
  Write-Host "=== Preflight: FFmpeg / Whisper ==="
  $pick = Select-BestFfmpeg $ffmpegParam

  Write-Host ""
  Write-Host "ffmpeg candidates:"
  $pick.All |
    Sort-Object -Property @{Expression='WhisperUsable';Descending=$true}, @{Expression='Major';Descending=$true} |
    Format-Table -AutoSize Path, Version, EnableWhisper, WhisperUsable |
    Out-Host

  $ffmpegPath = $pick.Selected.Path
  $ffprobePath = Select-Ffprobe $ffprobeParam $ffmpegPath

  $ver = Run-Cmd $ffmpegPath @("-hide_banner","-version")
  $txt = $ver.Stdout + "`n" + $ver.Stderr
  $v = Parse-Version $txt
  $cfg = Get-ConfigLine $txt

  Write-Host ("Using ffmpeg : {0}" -f $ffmpegPath)
  if ($v) { Print-Bool ("version major>=8 (" + $v.Raw + ")") ($v.Major -ge 8) } else { Print-Bool "version major>=8" $false }
  if ($cfg) { Print-Bool "--enable-whisper in configuration" ($cfg -match '--enable-whisper') }
  Print-Bool "whisper filter usable" ([bool]$pick.Selected.WhisperUsable)

  if (-not $pick.Selected.WhisperUsable) { throw "This ffmpeg does not have 'whisper' audio filter." }

  if ($ffprobePath) {
    Write-Host ("Using ffprobe: {0}" -f $ffprobePath)
  } else {
    Write-Host "[WARN] ffprobe not found."
  }
  Write-Host ""
  return [pscustomobject]@{ Ffmpeg=$ffmpegPath; Ffprobe=$ffprobePath }
}

function Find-ModelAuto([string]$rootDir) {
  $preferred = @(
    "ggml-large-v3-turbo-q5_0.bin",
    "ggml-large-v3-turbo.bin",
    "ggml-large-v3-q5_0.bin",
    "ggml-large-v3.bin",
    "ggml-large-v2-q5_0.bin",
    "ggml-large-v2.bin",
    "ggml-medium.bin",
    "ggml-small.bin",
    "ggml-base.bin",
    "ggml-tiny.bin"
  )
  $candidates = New-Object System.Collections.Generic.List[string]
  $candidates.Add((Get-Location).Path)
  if ($PSScriptRoot) { $candidates.Add($PSScriptRoot) }

  $hf = Join-Path $HOME ".cache\huggingface\hub\models--ggerganov--whisper.cpp\snapshots"
  if (Test-Path $hf) { $candidates.Add($hf) }

  if (Test-Path $rootDir) { $candidates.Add((Resolve-Path -LiteralPath $rootDir).Path) }

  foreach ($name in $preferred) {
    foreach ($dir in $candidates) {
      $hits = Get-ChildItem -LiteralPath $dir -Recurse -File -ErrorAction SilentlyContinue |
              Where-Object { $_.Name -ieq $name } |
              Sort-Object LastWriteTime -Descending
      if ($hits.Count -gt 0) { return $hits[0].FullName }
    }
  }

  foreach ($dir in $candidates) {
    $hits = Get-ChildItem -LiteralPath $dir -Recurse -File -ErrorAction SilentlyContinue |
            Where-Object { $_.Name -ilike "ggml-*.bin" } |
            Sort-Object LastWriteTime -Descending
    if ($hits.Count -gt 0) { return $hits[0].FullName }
  }

  throw "No ggml-*.bin model found. Put a model somewhere or pass -ModelPath."
}

function Get-DurationSec([string]$ffprobeExe, [string]$inputPath) {
  if (-not $ffprobeExe) { return $null }
  try {
    $dur = & $ffprobeExe -v error -show_entries format=duration -of default=noprint_wrappers=1:nokey=1 $inputPath 2>$null
    $dur = ($dur | Select-Object -First 1).Trim()
    if ($dur -match '^[0-9.]+$') { return [double]$dur }
  } catch {}
  return $null
}

function Get-FullPathNoExist([string]$p) {
  Throw-IfEmpty $p "path"
  $full = $p
  if (-not [System.IO.Path]::IsPathRooted($full)) {
    $full = Join-Path (Get-Location).Path $full
  }
  return [System.IO.Path]::GetFullPath($full)
}

function Escape-FfFilterPath([string]$p) {
  $full = Get-FullPathNoExist $p
  $full = $full -replace '\\','/'
  $full = $full -replace ':','\:'        # C:/... -> C\:/...
  $full = $full -replace "'","\\'"       # for '...'
  return $full
}

function Build-WhisperArgs(
  [string]$modelPath,
  [string]$outPath,
  [string]$language,
  [int]$queueSeconds,
  [bool]$useGpu,
  [int]$gpuDevice,
  [string]$vadModel,
  [double]$vadThreshold,
  [double]$vadMinSpeech,
  [double]$vadMinSilence
) {
  $m = Escape-FfFilterPath $modelPath
  $o = Escape-FfFilterPath $outPath
  $q = "{0}s" -f $queueSeconds

  $parts = @()
  $parts += "model='$m'"
  $parts += "destination='$o'"
  $parts += "format=srt"
  $parts += "language=$language"
  $parts += "queue=$q"
  $parts += "use_gpu=$($useGpu.ToString().ToLower())"
  $parts += "gpu_device=$gpuDevice"

  if (-not [string]::IsNullOrWhiteSpace($vadModel)) {
    $v = Escape-FfFilterPath $vadModel
    $parts += "vad_model='$v'"
    $parts += ("vad_threshold={0}" -f $vadThreshold)
    $parts += ("vad_min_speech_duration={0}s" -f $vadMinSpeech)
    $parts += ("vad_min_silence_duration={0}s" -f $vadMinSilence)
  }

  return ($parts -join ":")
}

function Invoke-FfmpegPerFile {
  param(
    [string]$FfmpegExe,
    [string]$FfprobeExe,
    [string]$ModelPathArg,
    [string]$VadModelArg,
    [string]$InputPath,
    [string]$OutputPath,
    [string]$LanguageArg,
    [int]$QueueSecondsArg,
    [bool]$UseGpu,
    [int]$GpuDeviceArg,
    [double]$VadThresholdArg,
    [double]$VadMinSpeechArg,
    [double]$VadMinSilenceArg,
    [bool]$ForceOverwrite,
    [int]$FilterThreadsArg,
    [hashtable]$Sync,
    [int]$TaskId
  )

  if (-not $ForceOverwrite -and (Test-Path -LiteralPath $OutputPath)) {
    return [pscustomobject]@{ TaskId=$TaskId; Status="skipped"; Input=$InputPath; Output=$OutputPath }
  }
  if ($ForceOverwrite -and (Test-Path -LiteralPath $OutputPath)) {
    Remove-Item -LiteralPath $OutputPath -Force -ErrorAction SilentlyContinue
  }

  $dur = Get-DurationSec $FfprobeExe $InputPath
  $Sync[$TaskId] = @{ file = [System.IO.Path]::GetFileName($InputPath); pct = 0; state="running" }

  $wh = Build-WhisperArgs $ModelPathArg $OutputPath $LanguageArg $QueueSecondsArg $UseGpu $GpuDeviceArg `
        $VadModelArg $VadThresholdArg $VadMinSpeechArg $VadMinSilenceArg

  $args = @("-hide_banner","-nostdin")
  if ($FilterThreadsArg -gt 0) { $args += @("-filter_threads", "$FilterThreadsArg") }
  $args += @("-i", $InputPath, "-vn", "-af", "whisper=$wh", "-f", "null", "NUL", "-progress", "pipe:1", "-loglevel", "error")

  $psi = New-Object System.Diagnostics.ProcessStartInfo
  $psi.FileName = $FfmpegExe
  $psi.Arguments = ($args | ForEach-Object { Quote-Arg $_ }) -join " "
  $psi.UseShellExecute = $false
  $psi.RedirectStandardOutput = $true
  $psi.RedirectStandardError  = $true
  $psi.CreateNoWindow = $true

  $p = New-Object System.Diagnostics.Process
  $p.StartInfo = $psi
  $null = $p.Start()

  while (-not $p.HasExited) {
    $line = $p.StandardOutput.ReadLine()
    if ($null -eq $line) { Start-Sleep -Milliseconds 10; continue }
    if ($line -match '^out_time_ms=(\d+)$') {
      if ($dur -and $dur -gt 0) {
        $sec = ([double]$matches[1]) / 1000000.0
        $pct = [math]::Max(0, [math]::Min(100, [math]::Floor(($sec / $dur) * 100)))
        $Sync[$TaskId] = @{ file = [System.IO.Path]::GetFileName($InputPath); pct = $pct; state="running" }
      }
    } elseif ($line -match '^progress=end$') {
      break
    }
  }

  $p.WaitForExit()
  $err = $p.StandardError.ReadToEnd()

  if ($p.ExitCode -ne 0) {
    $Sync[$TaskId] = @{ file = [System.IO.Path]::GetFileName($InputPath); pct = 0; state="failed" }
    return [pscustomobject]@{ TaskId=$TaskId; Status="failed"; Input=$InputPath; Output=$OutputPath; Error=$err }
  }

  $Sync[$TaskId] = @{ file = [System.IO.Path]::GetFileName($InputPath); pct = 100; state="done" }
  return [pscustomobject]@{ TaskId=$TaskId; Status="done"; Input=$InputPath; Output=$OutputPath }
}

function Invoke-FfmpegBatch {
  param(
    [string]$FfmpegExe,
    [string]$ModelPathArg,
    [string]$VadModelArg,
    [string[]]$InputPaths,
    [string[]]$OutputPaths,
    [string]$LanguageArg,
    [int]$QueueSecondsArg,
    [bool]$UseGpu,
    [int]$GpuDeviceArg,
    [double]$VadThresholdArg,
    [double]$VadMinSpeechArg,
    [double]$VadMinSilenceArg,
    [bool]$ForceOverwrite,
    [int]$FilterComplexThreadsArg,
    [hashtable]$Sync,
    [int]$TaskId
  )

  # remove outputs if Force
  for ($i=0; $i -lt $OutputPaths.Count; $i++) {
    if ($ForceOverwrite -and (Test-Path -LiteralPath $OutputPaths[$i])) {
      Remove-Item -LiteralPath $OutputPaths[$i] -Force -ErrorAction SilentlyContinue
    }
  }

  $Sync[$TaskId] = @{ file = ("batch(" + $InputPaths.Count + ")"); pct = 0; state="running" }

  $args = @("-hide_banner","-nostdin")
  if ($FilterComplexThreadsArg -gt 0) { $args += @("-filter_complex_threads", "$FilterComplexThreadsArg") }

  foreach ($inp in $InputPaths) { $args += @("-i", $inp) }

  $chains = New-Object System.Collections.Generic.List[string]
  $maps   = New-Object System.Collections.Generic.List[string]

  for ($i=0; $i -lt $InputPaths.Count; $i++) {
    $wh = Build-WhisperArgs $ModelPathArg $OutputPaths[$i] $LanguageArg $QueueSecondsArg $UseGpu $GpuDeviceArg `
          $VadModelArg $VadThresholdArg $VadMinSpeechArg $VadMinSilenceArg
    $chains.Add("[$i:a]whisper=$wh[a$i]")
    $maps.Add("-map")
    $maps.Add("[a$i]")
    $maps.Add("-f")
    $maps.Add("null")
    $maps.Add("NUL")
  }

  $filterComplex = ($chains -join ";")
  $args += @("-filter_complex", $filterComplex)
  $args += $maps
  $args += @("-loglevel","error")

  $psi = New-Object System.Diagnostics.ProcessStartInfo
  $psi.FileName = $FfmpegExe
  $psi.Arguments = ($args | ForEach-Object { Quote-Arg $_ }) -join " "
  $psi.UseShellExecute = $false
  $psi.RedirectStandardError  = $true
  $psi.RedirectStandardOutput = $true
  $psi.CreateNoWindow = $true

  $p = New-Object System.Diagnostics.Process
  $p.StartInfo = $psi
  $null = $p.Start()
  $p.WaitForExit()

  $err = $p.StandardError.ReadToEnd()

  if ($p.ExitCode -ne 0) {
    $Sync[$TaskId] = @{ file = ("batch(" + $InputPaths.Count + ")"); pct = 0; state="failed" }
    return [pscustomobject]@{ TaskId=$TaskId; Status="failed"; Error=$err; Count=$InputPaths.Count }
  }

  $Sync[$TaskId] = @{ file = ("batch(" + $InputPaths.Count + ")"); pct = 100; state="done" }
  return [pscustomobject]@{ TaskId=$TaskId; Status="done"; Count=$InputPaths.Count }
}

function Invoke-RunspacePool {
  param(
    [object[]]$WorkItems,
    [int]$Throttle,
    [scriptblock]$WorkerScript
  )

  $sync = [hashtable]::Synchronized(@{})
  $pool = [runspacefactory]::CreateRunspacePool(1, $Throttle)
  $pool.Open()

  $jobs = New-Object System.Collections.Generic.List[object]
  $total = $WorkItems.Count
  $completed = 0

  for ($i=0; $i -lt $WorkItems.Count; $i++) {
    $ps = [powershell]::Create()
    $ps.RunspacePool = $pool
    $null = $ps.AddScript($WorkerScript).AddArgument($WorkItems[$i]).AddArgument($i).AddArgument($sync)
    $handle = $ps.BeginInvoke()
    $jobs.Add([pscustomobject]@{ PS=$ps; Handle=$handle; Id=$i })
  }

  $results = New-Object System.Collections.Generic.List[object]

  while ($jobs.Count -gt 0) {
    for ($j = $jobs.Count - 1; $j -ge 0; $j--) {
      $job = $jobs[$j]
      if ($job.Handle.IsCompleted) {
        try {
          $out = $job.PS.EndInvoke($job.Handle)
          foreach ($o in $out) { $results.Add($o) }
        } finally {
          $job.PS.Dispose()
          $jobs.RemoveAt($j)
          $completed++
        }
      }
    }

    # progress (global + sample of running tasks)
    $running = $jobs.Count
    $pctDone = [math]::Floor(($completed / [math]::Max(1,$total)) * 100)

    $runningNames = @()
    foreach ($k in $sync.Keys) {
      $st = $sync[$k]
      if ($st -and $st.state -eq "running") { $runningNames += ("{0}({1}%)" -f $st.file, $st.pct) }
    }
    $show = ($runningNames | Select-Object -First 3) -join ", "
    $status = "done $completed/$total, running $running" + ($(if ($show) { " : $show" } else { "" }))

    Write-Progress -Id 1 -Activity "Whisper SRT (parallel/batch)" -Status $status -PercentComplete $pctDone
    Start-Sleep -Milliseconds 200
  }

  Write-Progress -Id 1 -Activity "Whisper SRT (parallel/batch)" -Completed

  $pool.Close()
  $pool.Dispose()

  return $results
}

# -------------------------
# Main
# -------------------------
$rootFull = (Resolve-Path -LiteralPath $Root).Path

$pf = Preflight $Ffmpeg $Ffprobe
$Ffmpeg  = $pf.Ffmpeg
$Ffprobe = $pf.Ffprobe

if (-not $ModelPath -or $ModelPath.Trim() -eq "") {
  $ModelPath = Find-ModelAuto $rootFull
}
if (-not (Test-Path -LiteralPath $ModelPath)) { throw "Model not found: $ModelPath" }

$useGpu = -not $NoGpu.IsPresent
if ($Parallelism -le 0) {
  if ($useGpu) { $Parallelism = 1 }
  else {
    $cpu = [Environment]::ProcessorCount
    $Parallelism = [math]::Max(1, [math]::Min($cpu, 4))
  }
}

Write-Host ("Root      : {0}" -f $rootFull)
Write-Host ("Model     : {0}" -f $ModelPath)
Write-Host ("Language  : {0}" -f $Language)
Write-Host ("Queue     : {0}s" -f $QueueSeconds)
Write-Host ("Use GPU   : {0} (device {1})" -f $useGpu, $GpuDevice)
Write-Host ("VAD       : {0}" -f $(if ($VadModelPath) { $VadModelPath } else { "(disabled)" }))
Write-Host ("Parallel  : {0}" -f $Parallelism)
Write-Host ("BatchSize : {0}" -f $BatchSize)
Write-Host ""

$extSet = New-Object 'System.Collections.Generic.HashSet[string]' ([StringComparer]::OrdinalIgnoreCase)
$Extensions | ForEach-Object { if (-not [string]::IsNullOrWhiteSpace($_)) { $extSet.Add($_.TrimStart('.')) | Out-Null } }

$allFiles = @(
  Get-ChildItem -LiteralPath $rootFull -File -Recurse:$Recurse |
  Where-Object { $extSet.Contains($_.Extension.TrimStart('.')) } |
  Sort-Object FullName
)

if ($allFiles.Count -eq 0) { Write-Host "No target files."; exit 0 }

# Build list of items that need work (skip existing unless -Force)
$todo = New-Object System.Collections.Generic.List[object]
foreach ($f in $allFiles) {
  $inPath = $f.FullName
  $outPath = [System.IO.Path]::ChangeExtension($inPath, ".srt")
  if (-not $Force -and (Test-Path -LiteralPath $outPath)) { continue }
  $todo.Add([pscustomobject]@{ In=$inPath; Out=$outPath })
}

if ($todo.Count -eq 0) { Write-Host "Nothing to do (all .srt exist)."; exit 0 }

# Group into batches
$work = New-Object System.Collections.Generic.List[object]
if ($BatchSize -le 1) {
  foreach ($t in $todo) { $work.Add([pscustomobject]@{ Type="file"; Inputs=@($t.In); Outputs=@($t.Out) }) }
} else {
  for ($i=0; $i -lt $todo.Count; $i += $BatchSize) {
    $chunk = $todo[$i..([math]::Min($todo.Count-1, $i+$BatchSize-1))]
    $ins  = @($chunk | ForEach-Object { $_.In })
    $outs = @($chunk | ForEach-Object { $_.Out })
    $work.Add([pscustomobject]@{ Type="batch"; Inputs=$ins; Outputs=$outs })
  }
}

$worker = {
  param($item, $taskId, $sync)

  # capture outer vars via $using: is not available here reliably -> pass via global scope variables
  $ffmpegExe  = $script:FfmpegExe
  $ffprobeExe = $script:FfprobeExe
  $model      = $script:Model
  $vadModel   = $script:VadModel
  $lang       = $script:Lang
  $queueSec   = $script:QueueSec
  $useGpu     = $script:UseGpuFlag
  $gpuDev     = $script:GpuDev
  $vadThr     = $script:VadThr
  $vadMinSp   = $script:VadMinSp
  $vadMinSi   = $script:VadMinSi
  $force      = $script:ForceFlag
  $ft         = $script:FT
  $fct        = $script:FCT

  if ($item.Type -eq "file") {
    return Invoke-FfmpegPerFile -FfmpegExe $ffmpegExe -FfprobeExe $ffprobeExe -ModelPathArg $model -VadModelArg $vadModel `
      -InputPath $item.Inputs[0] -OutputPath $item.Outputs[0] -LanguageArg $lang -QueueSecondsArg $queueSec -UseGpu $useGpu `
      -GpuDeviceArg $gpuDev -VadThresholdArg $vadThr -VadMinSpeechArg $vadMinSp -VadMinSilenceArg $vadMinSi -ForceOverwrite $force `
      -FilterThreadsArg $ft -Sync $sync -TaskId $taskId
  } else {
    return Invoke-FfmpegBatch -FfmpegExe $ffmpegExe -ModelPathArg $model -VadModelArg $vadModel -InputPaths $item.Inputs -OutputPaths $item.Outputs `
      -LanguageArg $lang -QueueSecondsArg $queueSec -UseGpu $useGpu -GpuDeviceArg $gpuDev -VadThresholdArg $vadThr -VadMinSpeechArg $vadMinSp `
      -VadMinSilenceArg $vadMinSi -ForceOverwrite $force -FilterComplexThreadsArg $fct -Sync $sync -TaskId $taskId
  }
}

# store vars for worker script
$script:FfmpegExe   = $Ffmpeg
$script:FfprobeExe  = $Ffprobe
$script:Model       = $ModelPath
$script:VadModel    = $VadModelPath
$script:Lang        = $Language
$script:QueueSec    = $QueueSeconds
$script:UseGpuFlag  = $useGpu
$script:GpuDev      = $GpuDevice
$script:VadThr      = $VadThreshold
$script:VadMinSp    = $VadMinSpeechSec
$script:VadMinSi    = $VadMinSilenceSec
$script:ForceFlag   = [bool]$Force
$script:FT          = $FilterThreads
$script:FCT         = $FilterComplexThreads

Write-Host ("Targets: {0} items (work units: {1})" -f $todo.Count, $work.Count)
Write-Host ""

$results = Invoke-RunspacePool -WorkItems $work.ToArray() -Throttle $Parallelism -WorkerScript $worker

$done = ($results | Where-Object { $_.Status -eq "done" }).Count
$sk  = ($results | Where-Object { $_.Status -eq "skipped" }).Count
$fail = ($results | Where-Object { $_.Status -eq "failed" }).Count

Write-Host ""
Write-Host "Finished."
Write-Host ("Done   : {0}" -f $done)
Write-Host ("Skipped: {0}" -f $sk)
Write-Host ("Failed : {0}" -f $fail)

if ($fail -gt 0) {
  Write-Host ""
  Write-Host "Failures (first 5):"
  $results | Where-Object { $_.Status -eq "failed" } | Select-Object -First 5 | ForEach-Object {
    if ($_.Input) { Write-Host ("- {0}" -f $_.Input) }
    if ($_.Error) { Write-Host ("  {0}" -f (($_.Error -split "`r?`n")[0])) }
  }
  exit 2
}

exit 0
