# Regenerates assets/licenses/third_party.json, the snapshot the in-app
# Licenses window (Help -> Licenses...) embeds at compile time.
#
# Two steps: cargo-about walks Cargo.lock and collects a licence text for every
# crate in the graph, then tools/gen-licenses merges that with the hand-written
# assets/licenses/extra.json (bundled C/C++ sources, installer DLLs, fonts,
# data, runtime-downloaded models) and pools the texts.
#
# Run it after any dependency change, then commit the regenerated JSON. The
# binary never generates this at build time, so a release build stays offline.
#
#   pwsh ./commands/generate_licenses.ps1
#
# cargo-about fails the run if a dependency introduces a licence that is not in
# the `accepted` list in about.toml. That is the point: an accidental GPL
# dependency should stop here rather than reach a release.

[CmdletBinding()]
param(
    # Skip the cargo-about pass and only re-merge, for when the crate graph has
    # not moved but extra.json or a file under texts/ has.
    [switch]$MergeOnly
)

$ErrorActionPreference = "Stop"

$repoRoot = Split-Path -Parent $PSScriptRoot
Push-Location $repoRoot
try {
    $rawJson = Join-Path $repoRoot "target/about-raw.json"

    if (-not $MergeOnly) {
        if (-not (Get-Command cargo-about -ErrorAction SilentlyContinue)) {
            Write-Error "cargo-about is not installed. Run: cargo install cargo-about --locked --features cli"
        }

        if (-not (Test-Path (Join-Path $repoRoot "vendor/signalsmith-stretch/Cargo.toml"))) {
            Write-Error "vendor/signalsmith-stretch is empty. Run: git submodule update --init --recursive"
        }

        New-Item -ItemType Directory -Force -Path (Join-Path $repoRoot "target") | Out-Null

        # `--features video` on top of the defaults: the snapshot is meant to
        # carry every licence text any build configuration could need, and the
        # Licenses window decides at runtime (via cfg!) which ones this binary
        # actually pulled in. Without it a default build's snapshot would be
        # missing OpenH264 entirely.
        Write-Host "==> cargo about generate (this walks the whole dependency graph)"
        cargo about generate --format json --features video -o $rawJson
        if ($LASTEXITCODE -ne 0) {
            Write-Error "cargo about failed. If it rejected a licence, decide whether to accept it in about.toml or drop the dependency."
        }
    }

    if (-not (Test-Path $rawJson)) {
        Write-Error "$rawJson not found. Run without -MergeOnly first."
    }

    Write-Host "==> merging with assets/licenses/extra.json"
    cargo run --release --quiet --manifest-path tools/gen-licenses/Cargo.toml -- `
        --raw $rawJson `
        --extra (Join-Path $repoRoot "assets/licenses/extra.json") `
        --texts (Join-Path $repoRoot "assets/licenses/texts") `
        --out   (Join-Path $repoRoot "assets/licenses/third_party.json")
    if ($LASTEXITCODE -ne 0) {
        Write-Error "gen-licenses failed."
    }

    Write-Host ""
    Write-Host "Wrote assets/licenses/third_party.json. Commit it, then run:"
    Write-Host "  cargo test --lib licenses"
}
finally {
    Pop-Location
}
