# CLI Migration Matrix

## Purpose

This table maps old automation entrypoints to the new `--cli` interface.

## Root Behavior

| Old | New | Notes |
| --- | --- | --- |
| `neowaves` | `neowaves` | GUI default remains unchanged |
| `neowaves --open-file ...` | `neowaves --open-file ...` | GUI compatibility path |
| `neowaves --open-session ...` | `neowaves --open-session ...` | GUI compatibility path |
| MCP stdio/http server | removed | replaced by `--cli` |

## MCP to CLI

| MCP capability | New CLI command | Notes |
| --- | --- | --- |
| list files | `neowaves --cli list query ...` | JSON rows instead of MCP tool result |
| get selection | `neowaves --cli editor selection get ...` | selection is now part of editor state |
| set selection | `neowaves --cli editor selection set ...` | supports samples or fractions |
| play/stop | `neowaves --cli editor playback play ...` | one-shot blocking playback only; no persistent transport control |
| set volume | deferred | not in Phase 1 |
| set speed/pitch/stretch | `neowaves --cli editor tool set ...` | apply via `editor tool apply` |
| apply gain | `neowaves --cli export file --gain-db ...` | direct headless export path |
| clear gain | `neowaves --cli editor tool set --tool gain --gain-db 0` | apply via `editor tool apply` |
| set loop markers | `neowaves --cli editor loop set ...` | non-destructive editor state |
| write loop markers | `neowaves --cli export file ...` | persistence is coupled to explicit export |
| export | `neowaves --cli export file ...` | JSON response with written path |
| open folder | `neowaves --cli list query --folder ...` | headless list source |
| open files | `neowaves --cli item inspect ...` | file-scoped commands |
| screenshot | `neowaves --cli render list|editor ...` or `debug screenshot` | render preferred, screenshot kept for compatibility |
| debug summary | `neowaves --cli debug summary ...` | JSON + summary text |

## GUI to CLI

| GUI area | New CLI family | Notes |
| --- | --- | --- |
| List table | `list query`, `list render` | list rows and list image |
| Editor inspector | `editor inspect` | JSON inspector replacement |
| Waveform editor image | `render editor --view-mode wave` | PNG output |
| Spectrogram image | `render spectrum` or `render editor --view-mode spec` | PNG output |
| Markers | `editor markers ...` | list/add/clear in Phase 1 |
| Loop | `editor loop ...` | get/set/clear/apply in Phase 1 |
| Session summary | `session inspect` | session metadata only |
| Artwork preview | `item artwork` | extracted PNG |
| Screenshot/debug summary | `debug screenshot`, `debug summary` | compatibility/debug surface |

## Legacy GUI Flags

| Flag | Status | Notes |
| --- | --- | --- |
| `--open-session` | kept | GUI startup compatibility |
| `--open-project` | kept | legacy GUI compatibility |
| `--open-folder` | kept | GUI startup compatibility |
| `--open-file` | kept | GUI startup compatibility |
| `--open-first` | kept | GUI startup compatibility |
| `--open-view-mode` | kept | GUI startup compatibility |
| `--waveform-overlay` | kept | GUI startup compatibility |
| `--screenshot` | kept | GUI compatibility/debug path |
| `--debug-summary` | kept | GUI compatibility/debug path |
| `--debug*` / `--auto-run*` | kept | GUI automation/debug path |
| `--mcp-*` | removed | replaced by `--cli` |

## Archive Plan

After MCP runtime removal:

- `docs/MCP実装ノウハウ.md` moves to `docs/archive/mcp/`
- MCP mentions in roadmap docs are rewritten as historical notes
- `commands/mcp_smoke.ps1` is removed
