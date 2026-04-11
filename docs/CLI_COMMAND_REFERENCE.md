# CLI Command Reference

## Conventions

- All commands start with `neowaves --cli`.
- JSON is written to stdout.
- Logs and diagnostics go to stderr.
- Relative input paths are accepted.
- Returned paths are absolute.

## Shared JSON Envelope

```json
{
  "ok": true,
  "command": "editor inspect",
  "result": {},
  "warnings": [],
  "errors": []
}
```

## session

### `session new`

Creates a session description from inputs without opening the GUI.

Example:

```powershell
neowaves --cli session new --folder .\assets\audio
```

Result shape:

- `root`
- `files`
- `count`

### `session inspect`

Reads a `.nwsess` file and returns its stored state summary.

Example:

```powershell
neowaves --cli session inspect --session .\work.nwsess
```

Result shape:

- `path`
- `version`
- `name`
- `base_dir`
- `file_count`
- `tab_count`
- `active_tab`
- `cached_edit_count`
- `selected_path`
- `sort`
- `search_query`
- `list_columns`

## item

### `item inspect`

Reads a single audio file and returns a combined summary.

Example:

```powershell
neowaves --cli item inspect --input .\demo.wav
```

Result shape:

- `path`
- `type`
- `meta`
- `markers`
- `loop`
- `artwork`

### `item meta`

Returns metadata only.

Example:

```powershell
neowaves --cli item meta --input .\demo.mp3
```

Result shape:

- `channels`
- `sample_rate`
- `bits_per_sample`
- `sample_value_kind`
- `bit_rate_bps`
- `duration_secs`
- `total_frames`
- `peak_db`
- `lufs_i`
- `bpm`

### `item artwork`

Extracts embedded artwork to a PNG file if available.

Example:

```powershell
neowaves --cli item artwork --input .\song.m4a --output .\out\art.png
```

Result shape:

- `path`
- `output`
- `width`
- `height`

## list

### `list columns`

Returns the stable column keys available to CLI list commands.

Example:

```powershell
neowaves --cli list columns
```

### `list query`

Returns structured list rows.

Inputs:

- `--folder <dir>` or `--session <file>`
- `--columns <csv>`
- `--sort-key <key>`
- `--sort-dir <asc|desc|none>`
- `--query <text>`
- `--limit <n>`
- `--offset <n>`
- `--include-overlays`

Example:

```powershell
neowaves --cli list query --folder .\assets\audio --columns file,length,sr,bits --limit 20
```

Result shape:

- `columns`
- `total`
- `offset`
- `limit`
- `rows`

Each row may include:

- `path`
- `display_name`
- `folder`
- `type`
- `meta`
- `pending_gain_db`
- `overlay`

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

Example:

```powershell
neowaves --cli list render --folder .\assets\audio --limit 40 --output .\out\list.png
```

Result shape:

- `path`
- `width`
- `height`
- `rows_rendered`
- `columns`

## editor

### `editor inspect`

Inspects editor state for a file or session-backed tab.

Inputs:

- `--input <audio>` or `--session <file> --path <audio>`

Example:

```powershell
neowaves --cli editor inspect --input .\demo.wav
```

Result shape:

- `path`
- `view_mode`
- `waveform_overlay`
- `zoom`
- `pan`
- `selection`
- `markers`
- `loop`
- `preview`
- `dirty`

### `editor view`

Gets or sets view state.

Supported actions:

- `get`
- `set`

Settable fields:

- `view_mode`
- `waveform_overlay`
- `samples_per_px`
- `view_offset`
- `vertical_zoom`
- `vertical_center`

### `editor selection`

Supported actions:

- `get`
- `set`
- `clear`

Set inputs:

- `--start-sample <n> --end-sample <n>`
- or `--start-frac <f> --end-frac <f>`

### `editor playback`

Phase 1b adds one-shot playback.

Supported actions:

- `play`

Playback inputs:

- `--input <audio>` or `--session <file> [--path <audio>]`
- `--selection`
- `--loop`
- `--start-sample <n> --end-sample <n>`
- or `--start-frac <f> --end-frac <f>`
- `--volume-db <db>`
- `--rate <speed>`
- `--output-device <name>`

Notes:

- playback runs only for the current CLI process
- range precedence is `selection > loop > explicit range > whole`
- exact-stream WAV is preferred when possible; otherwise buffer playback is used

Result shape:

- `path`
- `range`
- `duration_secs`
- `rate`
- `volume_db`
- `transport`
- `output_device`

### `editor tool`

Phase 1b adds active-tool inspection and mutation.

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

- `--tool <...>`
- `--fade-in-ms <ms>`
- `--fade-out-ms <ms>`
- `--gain-db <db>`
- `--normalize-target-db <db>`
- `--loudness-target-lufs <lufs>`
- `--pitch-semitones <semi>`
- `--stretch-rate <rate>`
- `--loop-repeat <count>`

Result shape:

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

- mutating marker commands require `--session`
- `set` replaces the full marker list
- `remove` deletes one marker by index
- `apply` updates the editor applied/committed baseline without writing the source file

Result shape:

- `current`
- `applied`
- `committed`
- `saved`
- `dirty`

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

- `mode`: `--mode <off|whole|marker>`
- `xfade`: `--samples <n> --shape <linear|equal|linear-dip|equal-dip>`
- `repeat`: `--count <n>`

Returned loop state contains:

- `current`
- `applied`
- `committed`
- `saved`
- `dirty`

## render

### `render waveform`

Renders a waveform PNG for a file.

Inputs:

- `--input <audio>`
- `--output <png>`
- `--width <px>`
- `--height <px>`
- `--mixdown`

### `render spectrum`

Renders a spectrum/spectrogram PNG for a file.

Inputs:

- `--input <audio>`
- `--output <png>`
- `--width <px>`
- `--height <px>`
- `--view-mode <spec|log|mel>`

### `render editor`

Renders the editor view to a PNG.

Inputs:

- `--input <audio>` or `--session <file> --path <audio>`
- `--output <png>`
- `--view-mode <wave|spec|log|mel|tempogram|chromagram>`
- `--waveform-overlay <on|off>`
- `--include-inspector`

### `render list`

Renders the list view to a PNG.

Inputs:

- `--folder <dir>` or `--session <file>`
- `--columns <csv>`
- `--offset <n>`
- `--limit <n>`
- `--output <png>`

## export

### `export file`

Exports one file through the current non-destructive export rules.

Inputs:

- standalone source: `--input <audio> --output <audio>`
- session-backed new file: `--session <file> [--path <audio>] --output <audio>`
- session-backed overwrite: `--session <file> [--path <audio>] --overwrite`
- `--format <wav|mp3|m4a|ogg>`
- `--gain-db <db>`
- `--loop-start-sample <n>`
- `--loop-end-sample <n>`
- `--marker <sample[:label]>`

Result shape:

- `source`
- `destination`
- `mode`
- `format`
- `saved_markers`
- `saved_loop`

## debug

### `debug summary`

Returns the same kind of debug summary text used by the GUI.

Inputs:

- `--input <audio>`
- `--session <file>`

### `debug screenshot`

Compatibility command that drives the GUI screenshot path and returns the written image path.

Inputs:

- same source selectors as `render list` / `render editor`
- `--output <png>`

## Notes

- Phase 1 intentionally omits `external`, `transcript`, `music-ai`, `plugin`, and `effect-graph`.
- Those domains get their own command namespaces in later phases without changing the root structure.
