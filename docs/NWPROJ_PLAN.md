# NeoWaves Session Save (.nwsess) Spec and Plan

## Goals
- File > Session Save / Open support.
- Save editor state (open tabs, edits-in-progress, selections, tool params, view state).
- Restore the session from a single `.nwsess` file.
- If a source file is missing, show an error on that item/tab.
- Session is optional: default workflow remains opening audio files directly.

## Non-Goals (for first release)
- Network or cloud sync.
- Cross-machine path rewriting beyond a simple relative-path option.
- Versioned history of edits inside a session.

---

## File Format (v1)
Use TOML (already in the repo) for a human-readable session file.

File: `MySession.nwsess`

```toml
version = 1
name = "My Session"
created_at = "2025-02-01T12:34:56Z"
base_dir = "C:\\Audio\\Samples"
open_first = true

[app]
theme = "dark"
list_sort_key = "File"
list_sort_dir = "None"
search_query = ""
search_regex = false
list_columns = { file = true, folder = true, transcript = false, external = false, length = true, ch = true, sr = true, bits = true, peak = true, lufs = true, gain = true, wave = true }

[spectrogram]
fft_size = 2048
window = "blackman_harris"
overlap = 0.875
max_frames = 4096
scale = "log"
mel_scale = "linear"
db_floor = -120.0
max_freq_hz = 0.0
show_note_labels = false

[[tabs]]
path = "voice\\line_001.wav"
missing = false
active_tool = "PitchShift"
view_mode = "Spectrogram"
show_waveform_overlay = false
channel_view = { mode = "mixdown", selected = [] }
tool_state = { fade_in_ms = 0.0, fade_out_ms = 0.0, gain_db = 0.0, normalize_target_db = -6.0, pitch_semitones = 3.0, stretch_rate = 1.0 }
loop = { mode = "Off", region = [0, 0], xfade_samples = 0, xfade_shape = "EqualPower" }
trim_range = [0, 0]
selection = [0, 0]
markers = [{ sample = 1234, label = "M01" }]
dirty = true
edited_audio = "data/tab_0001.wav"

[[tabs]]
path = "missing\\file.wav"
missing = true
error = "Source file missing"
```

### Notes
- `base_dir`: used to resolve relative `tabs.path`. If absolute, keep as-is.
- `edited_audio`: optional sidecar file for edited waveform (see below).
- `missing`: set on load, not necessarily written on save (runtime check).

---

## Edited Waveform Storage
Current edits are destructive to in-memory samples. To restore "edited waveform" we must persist it.

### v1 approach
- Create a sidecar folder next to the session file:
  - `MySession.nwsess.d/`
- Save edited audio per tab:
  - `data/tab_0001.wav` (32-bit float WAV, matching sample rate + channels).
- `edited_audio` in the session file points to this path.
- If a tab is not dirty, omit `edited_audio` to avoid bloat.

### Rationale
- Fast restore and exact waveform reproduction.
- Avoids re-running heavy edits (pitch/time-stretch).

---

## Missing Source Files
On load:
- If `tabs.path` cannot be found, create a placeholder item:
  - Show in list with "[Missing]" prefix and a warning color.
  - In editor, display a "Source file missing" banner.
- If `edited_audio` exists, allow editor to open the edited audio as a virtual track.
- If both source and edited audio are missing, keep the placeholder and show error only.

---

## UI/UX
Add to File menu:
- Session Save...
- Session Save As...
- Session Open...
- Session Close (clears session and returns to list view)

Behavior:
- Session Save defaults to last session path if already opened.
- Save As chooses path and writes sidecar folder if needed.
- On opening a session, restore the list, tabs, and editor state. Show a one-line toast "Session loaded".

---

## Data to Capture
- List state: root folder, sort key/dir, search query/regex, list columns.
- Open tabs: order, active tab, view mode, channel view, tool state, selection, markers, loop, trim, fade.
- Global editor options: spectrogram config.
- Dirty flag per tab + edited audio sidecar when dirty.

---

## Implementation Plan

### Phase 1: Data model + serialization
- Add `SessionFile` structs in `src/app/types.rs` (serde-friendly).
- Implement `read_session(path)` and `write_session(path)` in `src/app/project.rs` (module name is legacy).
- Use TOML for v1 serialization.

### Phase 2: Save / Open wiring
- File menu actions:
  - `Session Save` / `Save As` / `Open`.
- Save:
  - Gather state, serialize to `.nwsess`.
  - Write edited audio sidecars for dirty tabs.
- Open:
  - Clear current session safely.
  - Load list, tabs, and editor state.
  - Missing files create placeholders + warnings.

### Phase 3: Missing file UX + fallback
- Add list-level "missing source" badge.
- Editor banner with clear error message.
- Allow editing if `edited_audio` is present; otherwise read-only placeholder.

---

## Follow-ups (v2)
- Optional "Save edits" checkbox to avoid sidecar audio.
- Portable sessions (path remapping dialog).
- Compact storage using FLAC or OGG for edited audio.

