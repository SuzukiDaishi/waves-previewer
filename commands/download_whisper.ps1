# Download 2 files into the default Hugging Face Hub cache layout (refs/blobs/snapshots)
# without using hf CLI (PowerShell + .NET only).

$ErrorActionPreference = "Stop"

function Get-HfHubCacheDir {
    if ($env:HF_HUB_CACHE -and $env:HF_HUB_CACHE.Trim() -ne "") {
        return $env:HF_HUB_CACHE
    }
    if ($env:HF_HOME -and $env:HF_HOME.Trim() -ne "") {
        return (Join-Path $env:HF_HOME "hub")
    }
    return (Join-Path $HOME ".cache\huggingface\hub")
}

function Get-HfNoRedirectMetadata {
    param(
        [Parameter(Mandatory=$true)][string]$ResolveUrl
    )

    # No-redirect request + Range 0-0 to fetch headers cheaply (like many HF clients do).
    $handler = [System.Net.Http.HttpClientHandler]::new()
    $handler.AllowAutoRedirect = $false

    $client = [System.Net.Http.HttpClient]::new($handler)
    $client.Timeout = [TimeSpan]::FromSeconds(30)

    $req = [System.Net.Http.HttpRequestMessage]::new([System.Net.Http.HttpMethod]::Get, $ResolveUrl)
    $req.Headers.Range = [System.Net.Http.Headers.RangeHeaderValue]::new(0, 0)

    $resp = $client.SendAsync($req).GetAwaiter().GetResult()

    function Get-HeaderValue([System.Net.Http.HttpResponseMessage]$r, [string[]]$names) {
        foreach ($n in $names) {
            if ($r.Headers.Contains($n)) { return ($r.Headers.GetValues($n) | Select-Object -First 1) }
            if ($r.Content.Headers.Contains($n)) { return ($r.Content.Headers.GetValues($n) | Select-Object -First 1) }
        }
        return $null
    }

    $commit = Get-HeaderValue $resp @("X-Repo-Commit", "x-repo-commit")
    $etag   = Get-HeaderValue $resp @("X-Linked-Etag", "x-linked-etag", "ETag", "etag")

    if ($etag) { $etag = $etag.Trim('"') }  # HF clients commonly strip quotes

    $client.Dispose()
    $handler.Dispose()

    [pscustomobject]@{
        Commit = $commit
        ETag   = $etag
        Status = [int]$resp.StatusCode
    }
}

function Download-UrlToFile {
    param(
        [Parameter(Mandatory=$true)][string]$Url,
        [Parameter(Mandatory=$true)][string]$OutFile
    )

    $outDir = Split-Path -Parent $OutFile
    if (-not (Test-Path $outDir)) { New-Item -ItemType Directory -Force -Path $outDir | Out-Null }

    $tmp = "$OutFile.partial"
    if (Test-Path $tmp) { Remove-Item -Force $tmp }

    $handler = [System.Net.Http.HttpClientHandler]::new()
    $handler.AllowAutoRedirect = $true

    $client = [System.Net.Http.HttpClient]::new($handler)
    # Big files: no short timeout
    $client.Timeout = [TimeSpan]::FromDays(1)

    $resp = $client.GetAsync($Url, [System.Net.Http.HttpCompletionOption]::ResponseHeadersRead).GetAwaiter().GetResult()
    $resp.EnsureSuccessStatusCode() | Out-Null

    $inStream = $resp.Content.ReadAsStreamAsync().GetAwaiter().GetResult()
    $outStream = [System.IO.File]::Open($tmp, [System.IO.FileMode]::CreateNew, [System.IO.FileAccess]::Write, [System.IO.FileShare]::None)

    try {
        $inStream.CopyTo($outStream)
    } finally {
        $outStream.Dispose()
        $inStream.Dispose()
        $client.Dispose()
        $handler.Dispose()
    }

    Move-Item -Force $tmp $OutFile
}

function Ensure-HfCachedFile {
    param(
        [Parameter(Mandatory=$true)][string]$RepoId,     # e.g. ggerganov/whisper.cpp
        [Parameter(Mandatory=$true)][string]$Revision,   # e.g. main
        [Parameter(Mandatory=$true)][string]$FilePathInRepo  # e.g. ggml-large-v3-turbo.bin
    )

    $cacheRoot = Get-HfHubCacheDir

    # huggingface_hub cache layout: models--<namespace>--<name>
    $repoFolderName = "models--" + ($RepoId -replace "/", "--")
    $repoRoot = Join-Path $cacheRoot $repoFolderName

    $blobsDir = Join-Path $repoRoot "blobs"
    $refsDir  = Join-Path $repoRoot "refs"
    $snapsDir = Join-Path $repoRoot "snapshots"

    New-Item -ItemType Directory -Force -Path $blobsDir, $refsDir, $snapsDir | Out-Null

    $resolveUrl = "https://huggingface.co/$RepoId/resolve/$Revision/$FilePathInRepo"

    $meta = Get-HfNoRedirectMetadata -ResolveUrl $resolveUrl

    if (-not $meta.ETag) {
        throw "Could not read ETag from server headers. Try using 'hf download' instead, or check network/proxy."
    }
    if (-not $meta.Commit) {
        throw "Could not read commit hash (X-Repo-Commit) from server headers. Try using 'hf download'."
    }

    $blobPath = Join-Path $blobsDir $meta.ETag
    if (-not (Test-Path $blobPath)) {
        Write-Host "Downloading blob -> $blobPath"
        Download-UrlToFile -Url $resolveUrl -OutFile $blobPath
    } else {
        Write-Host "Already cached blob: $blobPath"
    }

    # refs/<revision> = <commit>
    $refPath = Join-Path $refsDir $Revision
    Set-Content -NoNewline -Encoding ASCII -Path $refPath -Value $meta.Commit

    # snapshots/<commit>/<file> -> ../../blobs/<etag>  (hardlink if possible, else copy)
    $snapCommitDir = Join-Path $snapsDir $meta.Commit
    New-Item -ItemType Directory -Force -Path $snapCommitDir | Out-Null

    $snapFilePath = Join-Path $snapCommitDir $FilePathInRepo
    if (-not (Test-Path $snapFilePath)) {
        try {
            # Prefer hardlink (no admin needed). If it fails, fall back to copy.
            New-Item -ItemType HardLink -Path $snapFilePath -Target $blobPath | Out-Null
        } catch {
            Copy-Item -Force -Path $blobPath -Destination $snapFilePath
        }
    }

    return $snapFilePath
}

# ---- Targets ----
$repo = "ggerganov/whisper.cpp"
$rev  = "main"

$paths = @()
$paths += Ensure-HfCachedFile -RepoId $repo -Revision $rev -FilePathInRepo "ggml-large-v3-turbo.bin"
$paths += Ensure-HfCachedFile -RepoId $repo -Revision $rev -FilePathInRepo "ggml-large-v3-turbo-q5_0.bin"

Write-Host ""
Write-Host "Done. Snapshot paths (same style as hf download):"
$paths | ForEach-Object { Write-Host $_ }
Write-Host ""
Write-Host "Hub cache root:"
Write-Host (Get-HfHubCacheDir)
