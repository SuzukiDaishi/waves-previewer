# CLI Command Reference

## Conventions

- All headless commands start with `neowaves --cli`.
- `stdout` is always JSON.
- Human-readable logs and errors go to `stderr`.
- Returned paths are absolute.
- Read-only commands may use `--input` or `--folder`.
- Mutating editor and batch commands are session-backed and require `--session`.
- Session-backed render/export commands use the session's current state, not only the source file baseline.

## Shared JSON Envelope

```json
{
  "ok": true,
  "command": "list.query",
  "result": {},
  "warnings": [],
  "errors": []
}
```

## Stable Handles

- `query_id`
  - Replays a saved list query/filter/sort combination.
- `row_id`
  - Stable path-based handle returned from `list query` rows and selection results.
- `graph_node_id`
  - Existing node `id` inside an effect graph.
- `graph_edge_id`
  - Existing edge `id` inside an effect graph.

## session

### `session new`

Creates a `.nwsess` file from a folder or explicit file list.

Examples:

```powershell
neowaves --cli session new --folder .\assets\audio --output .\work.nwsess
neowaves --cli session new --input .\a.wav --input .\b.wav --output .\work.nwsess
```

Result highlights:

- `session_path`
- `file_count`
- `files`
- `open_first`

### `session inspect`

Reads a `.nwsess` file and returns its stored state summary.

Example:

```powershell
neowaves --cli session inspect --session .\work.nwsess
```

Result highlights:

- `session_path`
- `version`
- `base_dir`
- `file_count`
- `tab_count`
- `active_tab`
- `selected_path`
- `sort`
- `search_query`
- `list_columns`

## item

### `item inspect`

Returns a combined item summary.

Example:

```powershell
neowaves --cli item inspect --input .\demo.wav
```

Result highlights:

- `path`
- `meta`
- `markers`
- `loop`
- `artwork`

### `item meta`

Returns metadata only.

### `item artwork`

Extracts embedded artwork to PNG if available.

## list

### `list columns`

Returns stable CLI list column keys.

### `list query`

Returns structured list rows.

Inputs:

- `--folder <dir>` or `--session <file>`
- `--columns <csv>`
- `--query <text>`
- `--sort-key <key>`
- `--sort-dir <asc|desc|none>`
- `--query-id <id>`
- `--offset <n>`
- `--limit <n>`
- `--include-overlays`

Example:

```powershell
neowaves --cli list query --session .\work.nwsess --query _BGM --columns file,folder,lufs,gain,wave
```

Result highlights:

- `query_id`
- `total`
- `offset`
- `limit`
- `columns`
- `rows`

Each row may include:

- `row_id`
- `path`
- `file`
- `folder`
- `gain`
- `length`
- `channels`
- `sample_rate`
- `bits`
- `bit_rate`
- `overlay`

### `list sort`

Stores sort state into the session, then returns `list query` output.

Example:

```powershell
neowaves --cli list sort --session .\work.nwsess --sort-key lufs --sort-dir asc
```

### `list search`

Stores search text into the session, then returns `list query` output.

### `list select`

Stores a selected row into the session.

Inputs:

- explicit path: `--path <audio>`
- or query-based selection: `--query <text> --index <n>`

Result highlights:

- `selected_path`
- `selected_row_id`

### `list save-query`

Returns a reusable `query_id` for later `list` or `batch` calls.

Example:

```powershell
neowaves --cli list save-query --session .\work.nwsess --query _BGM --sort-key lufs --sort-dir asc
```

### `list render`

Renders a list image to PNG.

Inputs:

- `--folder <dir>` or `--session <file>`
- `--columns <csv>`
- `--offset <n>`
- `--limit <n>`
- `--output <png>`
- `--show-markers`
- `--show-loop`

Result highlights:

- `path`
- `width`
- `height`
- `source`

## batch

### `batch loudness plan`

Measures loudness for the matched session rows and proposes pending gain changes.

Inputs:

- `--session <file>`
- `--query <text>` or `--query-id <id>`
- `--sort-key <key>`
- `--sort-dir <dir>`
- `--target-lufs <value>`
- `--report <path>`

Example:

```powershell
neowaves --cli batch loudness plan --session .\work.nwsess --query _BGM --target-lufs -24 --report .\bgm_plan.md
```

Result highlights:

- `query_id`
- `matched_paths`
- `target_lufs`
- `rows`

Each row includes:

- `row_id`
- `path`
- `measured_lufs`
- `effective_lufs`
- `existing_gain_db`
- `proposed_gain_db`
- `clipping_risk`
- `warning`

### `batch loudness apply`

Applies the proposed loudness correction into the session's pending gain state only.

Result highlights:

- `before`
- `after`
- `mutated_paths`
- `skipped_paths`
- `failed_paths`
- `pending_gain_db`

### `batch export`

Exports the session rows matched by `--query` or `--query-id`.

Inputs:

- `--session <file>`
- exactly one of `--overwrite` or `--output-dir <dir>`
- `--query <text>` or `--query-id <id>`
- `--report <path>`

Result highlights:

- `before`
- `after`
- `mutated_paths`
- `skipped_paths`
- `failed_paths`

## editor

### `editor inspect`

Returns editor state for an input file or a session-backed tab.

Example:

```powershell
neowaves --cli editor inspect --session .\work.nwsess --path .\battle.wav
```

Result highlights:

- `file_meta`
- `view`
- `cursor`
- `selection`
- `markers`
- `loop`
- `tool`
- `dirty`

### `editor view`

Supported actions:

- `get`
- `set`

Settable fields:

- `--view-mode`
- `--waveform-overlay`
- `--samples-per-px`
- `--view-offset`
- `--vertical-zoom`
- `--vertical-center`

### `editor selection`

Supported actions:

- `get`
- `set`
- `clear`

### `editor cursor`

Supported actions:

- `get`
- `set`
- `nudge`

Inputs:

- `--sample <n>`
- `--frac <f>`
- `--samples <delta>`
- `--snap none|zero-cross`

### `editor playback play`

One-shot blocking playback.

Range precedence:

1. `--selection`
2. `--loop`
3. explicit sample or fraction range
4. whole file

Inputs:

- `--input <audio>` or `--session <file> [--path <audio>]`
- `--selection`
- `--loop`
- `--start-sample <n> --end-sample <n>`
- `--start-frac <f> --end-frac <f>`
- `--volume-db <db>`
- `--rate <speed>`
- `--output-device <name>`

Result highlights:

- `path`
- `range`
- `duration_secs`
- `rate`
- `volume_db`
- `transport`
- `output_device`

### `editor tool`

Supported actions:

- `get`
- `set`
- `apply`

Supported tool values:

- `trim`
- `fade`
- `pitch`
- `stretch`
- `gain`
- `normalize`
- `loudness`
- `reverse`

Settable fields:

- `--tool`
- `--fade-in-ms`
- `--fade-out-ms`
- `--gain-db`
- `--normalize-target-db`
- `--loudness-target-lufs`
- `--pitch-semitones`
- `--stretch-rate`
- `--loop-repeat`

Result highlights:

- `active_tool`
- `tool_state`
- `dirty`

### `editor markers`

Supported actions:

- `list`
- `add`
- `set`
- `remove`
- `clear`
- `apply`

Notes:

- mutating commands require `--session`
- `set` replaces the full marker list
- `apply` commits the current session marker state without exporting audio

### `editor loop`

Supported actions:

- `get`
- `set`
- `clear`
- `apply`
- `mode`
- `xfade`
- `repeat`

Set inputs:

- `--start-sample <n> --end-sample <n>`
- or `--start-frac <f> --end-frac <f>`

Additional inputs:

- `mode`: `--mode off|whole|marker`
- `xfade`: `--samples <n> --shape linear|equal|linear-dip|equal-dip`
- `repeat`: `--count <n>`

Returned state highlights:

- `current`
- `applied`
- `committed`
- `saved`
- `dirty`

## render

### `render waveform`

Renders a waveform PNG from either raw input or session current state.

Inputs:

- `--input <audio>` or `--session <file> [--path <audio>]`
- `--output <png>`
- `--width <px>`
- `--height <px>`
- `--mixdown`
- `--selection`
- `--loop`
- `--start-sample <n> --end-sample <n>`
- `--start-frac <f> --end-frac <f>`
- `--show-markers`
- `--show-loop`

Result highlights:

- `path`
- `width`
- `height`
- `source`
- `view_params`

### `render spectrum`

Renders a spectrum/spectrogram PNG for a file.

### `render editor`

Renders the editor view to PNG.

### `render list`

Renders the list view to PNG.

## export

### `export file`

Exports one file using current non-destructive state.

Modes:

- standalone new file: `--input <audio> --output <audio>`
- session-backed new file: `--session <file> [--path <audio>] --output <audio>`
- session-backed overwrite: `--session <file> [--path <audio>] --overwrite`

Inputs:

- `--format <wav|mp3|m4a|ogg>`
- `--gain-db <db>`
- `--loop-start-sample <n>`
- `--loop-end-sample <n>`
- `--marker <sample[:label]>`

Result highlights:

- `source`
- `destination`
- `mode`
- `saved_markers`
- `saved_loop`
- `loop_verification`

### `export verify-loop-tags`

Reads loop and marker metadata back from an exported file.

Example:

```powershell
neowaves --cli export verify-loop-tags --input .\out\music_interactive.mp3
```

Result highlights:

- `path`
- `format`
- `sample_rate`
- `marker_count`
- `markers`
- `has_loop_region`
- `loop_region`

## effect-graph

### `effect-graph list`

Lists installed effect graph templates with validation summaries.

### `effect-graph new`

Creates a new graph file in the template library or at an explicit output path.

### `effect-graph inspect`

Returns the full graph JSON plus validation summary.

### `effect-graph render`

Renders a schematic PNG preview of the graph.

### `effect-graph validate`

Returns machine-readable validation results and optional report output.

### `effect-graph test`

Runs the graph against an input file or an embedded sample.

Result highlights:

- `output_channels`
- `output_sample_rate`
- `per_channel_peak_db`
- `silent_outputs`
- `debug_preview`
- `rendered_preview_path`

### `effect-graph save`

Touches the graph on disk and updates its metadata timestamp.

### `effect-graph import`

Imports a graph JSON file into the template library or a specified destination.

### `effect-graph export`

Copies a graph JSON file to a chosen destination.

### `effect-graph node`

Supported actions:

- `add`
- `remove`
- `set`

Used for graph node authoring without inventing a separate DSL.

### `effect-graph edge`

Supported actions:

- `connect`
- `disconnect`

Used for routing edits by node id and port id.

## external

All `external` mutations are session-backed.

### `external inspect`

Returns the current external merge state for a session.

Example:

```powershell
neowaves --cli external inspect --session .\work.nwsess
```

Result highlights:

- `sources`
- `active_source`
- `headers`
- `visible_columns`
- `key_rule`
- `regex`
- `match_count`
- `unmatched_count`

### `external render`

Renders the merged external preview to PNG.

Example:

```powershell
neowaves --cli external render --session .\work.nwsess --output .\external_preview.png
```

Result highlights:

- `path`
- `width`
- `height`
- `matched_rows`
- `unmatched_rows`

### `external rows`

Returns merged rows and unmatched-row previews as JSON.

Inputs:

- `--session <file>`
- `--offset <n>`
- `--limit <n>`
- `--include-unmatched`

Result highlights:

- `headers`
- `matched_rows`
- `unmatched_rows`

### `external source`

Supported actions:

- `list`
- `add`
- `reload`
- `remove`
- `clear`

`add` and `reload` inputs:

- `--input <csv|xlsx>`
- `--sheet <name>`
- `--has-header on|off`
- `--header-row <n>`
- `--data-row <n>`

Example:

```powershell
neowaves --cli external source add --session .\work.nwsess --input .\meta.xlsx --sheet Sheet1
```

Mutating result highlights:

- `before`
- `after`
- `mutated_sources`
- `warnings`

### `external config`

Supported actions:

- `get`
- `set`

Settable fields:

- `--key-column`
- `--key-rule file|stem|regex`
- `--regex-input file|stem|path|dir`
- `--regex-pattern <pattern>`
- `--regex-replace <text>`
- `--scope-regex <pattern>`
- `--visible-columns <csv>`
- `--show-unmatched`

## transcript

### `transcript inspect`

Returns transcript segments and transcript file metadata.

Inputs:

- `--input <audio>`
- or `--session <file> [--path <audio>]`

Example:

```powershell
neowaves --cli transcript inspect --session .\work.nwsess
```

Result highlights:

- `path`
- `srt_path`
- `language`
- `segment_count`
- `segments`
- `full_text`

### `transcript model`

Supported actions:

- `status`
- `download`
- `uninstall`

Result highlights:

- `model_dir`
- `available`
- `variants`
- `vad_available`

### `transcript config`

Supported actions:

- `get`
- `set`

Settable fields:

- `--language`
- `--task transcribe|translate`
- `--perf-mode fast|balanced|accurate`
- `--model-variant tiny|base|small|medium|large-v3`
- `--compute-target auto|cpu|gpu`
- `--vad on|off`
- `--vad-threshold <f>`
- `--overwrite-existing`

### `transcript generate`

Runs AI transcription for one target.

Example:

```powershell
neowaves --cli transcript generate --session .\work.nwsess --write-srt --overwrite-existing
```

Result highlights:

- `path`
- `srt_path`
- `segment_count`
- `language`
- `warnings`

### `transcript batch generate`

Runs transcription for a query or saved `query_id`.

Result highlights:

- `completed_paths`
- `skipped_paths`
- `failed_paths`
- `output_srt_paths`

### `transcript export-srt`

Writes transcript state to an explicit `.srt` path.

## music-ai

### `music-ai inspect`

Returns the current music-analysis result stored in the session.

Result highlights:

- `path`
- `source_kind`
- `estimated_bpm`
- `beats`
- `downbeats`
- `sections`
- `stems_ready`
- `last_error`

### `music-ai model`

Supported actions:

- `status`
- `download`
- `uninstall`

Result highlights:

- `analysis_model_available`
- `demucs_available`
- `model_dir`

### `music-ai analyze`

Runs the music-analysis pipeline on the session target.

Inputs:

- `--session <file>`
- `--path <audio>`
- `--stems-dir <dir>`
- `--prefer-demucs`
- `--report <path>`

Example:

```powershell
neowaves --cli music-ai analyze --session .\work.nwsess --report .\music_analysis.md
```

Result highlights:

- `before`
- `after`
- `estimated_bpm`
- `beat_count`
- `downbeat_count`
- `section_count`
- `report_path`

### `music-ai apply-markers`

Copies analysis events into the current marker list.

Inputs:

- `--beats`
- `--downbeats`
- `--sections`
- `--replace`

Result highlights:

- `before`
- `after`
- `generated_markers`

### `music-ai export-stems`

Exports generated stems to a destination directory.

Result highlights:

- `exported_files`
- `failed_paths`
- `destination_dir`

## plugin

### `plugin search-path`

Supported actions:

- `list`
- `add`
- `remove`
- `reset`

This controls the same search-path state used by the GUI scan flow.

### `plugin scan`

Rebuilds the plugin catalog.

Result highlights:

- `catalog_size`
- `search_paths`
- `warnings`

### `plugin list`

Returns catalog entries.

Inputs:

- `--filter <text>`
- `--limit <n>`

Each row may include:

- `plugin_key`
- `name`
- `backend`
- `format`
- `vendor`
- `category`
- `path`

### `plugin probe`

Reads plugin capabilities and parameter information.

Example:

```powershell
neowaves --cli plugin probe --plugin "C:\Program Files\Common Files\VST3\OTT.vst3"
```

Result highlights:

- `plugin_key`
- `backend`
- `capabilities`
- `params`
- `backend_note`
- `fallback_hint`

### `plugin session`

Supported actions:

- `inspect`
- `set`
- `preview`
- `apply`
- `clear`

Notes:

- all actions require `--session`
- `set` updates the current plugin draft for the target tab
- `preview` runs the existing preview worker headlessly
- `apply` commits the plugin output into current edited audio
- `clear` removes the draft

Result highlights:

- `before`
- `after`
- `plugin_fx_draft`
- `preview_overlay_ready`
- `mutated_paths`

## debug

### `debug summary`

Runs the GUI-compatible debug summary path and returns the saved text path.

### `debug screenshot`

Runs the GUI-compatible screenshot path and returns the written image path.

## Current Scope

Implemented and usable today:

- `session`
- `item`
- `list`
- `batch`
- `editor`
- `render`
- `export`
- `effect-graph`
- `external`
- `transcript`
- `music-ai`
- `plugin`
- `debug`
