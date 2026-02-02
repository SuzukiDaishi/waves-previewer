# Major Update Plan (2026-01-28)

This document covers the requested fixes and new features. It is scoped to keep the list view fast, preserve non-destructive editing until Save, and avoid UI stalls.

## Goals
- Multi-source external data (CSV/Excel) must merge and display all rows/columns.
- Text input/IME focus must never trigger unintended editor open.
- OS file association open must always load files in list/editor.
- Editor navigation improvements (up/down zoom, zero-cross movement already done).
- Apply semantics consistent across all tools (preview -> applied memory -> destructive save).
- Clipboard hotkeys reliability.
- New features: loudness normalize, sample-rate convert (non-destructive), time axis in spectrogram.
- Remove Command Palette feature and simplify the header layout.
- Use "Session" naming consistently for state save/open, and separate Save vs Export semantics.

## Phase 1: External Data (multi CSV/Excel)
### Requirements
- Multiple CSV/Excel sources can be loaded simultaneously.
- Columns are unioned; rows merged by key (regex rule).
- Same column + same key: last loaded wins.
- Unmatched rows still optionally shown and can be filtered.

### Implementation Plan
1) Data model
   - Replace single `external_source/headers/rows` with a list of sources.
   - Each source stores: path, sheet, headers, rows, row_count, load order, load settings.
2) Merge layer
   - Build a merged view table from all sources in load order.
   - Keyed by external key (regex rule). Use `HashMap<key, row>`; later sources overwrite cells.
   - Keep a merged list of columns (stable order by first appearance, then new columns appended).
3) UI
   - External Data window shows a source list (add/remove/reload).
   - Columns list reflects merged columns; optional per-source diagnostics.
4) Performance
   - Keep per-source raw rows (for quick re-merge).
   - Cache merged rows and only re-merge on settings or source changes.
   - Avoid allocations during draw by precomputing merged row strings.

### Acceptance
- Loading `a.csv` then `b.csv` produces rows for `aaa..ggg` with `ddd=55`.
- Columns from both appear in list, in stable order.

## Phase 2: Focus / IME / Search enter
### Requirements
- Enter to confirm IME input must NOT open editor.

### Implementation Plan
1) Track UI focus from egui:
   - When any text input is focused, set `suppress_list_enter = true` for that frame.
   - Use `ctx.wants_keyboard_input()` plus explicit focus checks on search box.
2) List enter logic
   - Make list open conditional on `!suppress_list_enter`.
   - Keep current behavior for keyboard list navigation.

### Acceptance
- With Japanese IME, Enter only commits text; no editor open.

## Phase 3: OS File Association Open
### Requirements
- Double-click single file opens editor + loads file.
- Multi-select files open in one instance and load all.
- If app is already open, files are added to that instance.

### Implementation Plan
1) CLI parsing: accept raw file args (already done in `main.rs`).
2) Single-instance IPC: send file list to existing instance (already done).
3) Post-open behavior:
   - After add, if a single file was opened, auto-open tab for that file.
   - For multi-file open, list shows all; editor does not auto-open unless requested.

### Acceptance
- Double-click a single wav opens editor on that file.
- Multi-select opens one instance with all files added.

## Phase 4: Editor Zoom with Up/Down
### Requirements
- Waveform view: Up/Down changes zoom.

### Implementation Plan
1) Add Up/Down handling in editor view (when not in text input).
2) Adjust `samples_per_px` with bounds and keep center anchored.
3) Respect long-press behavior similar to left/right navigation.

### Acceptance
- Up zooms in, Down zooms out, smooth and anchored.

## Phase 5: Apply Semantics (all tools)
### Requirements
- All inspector editing uses preview (yellow-green), then Apply => committed memory, Save => destructive.
- Apply resets tool parameters to defaults.
- Markers and loop follow same pipeline.
- Markers table adds status column (● pending/applied/saved).

### Implementation Plan
1) Tool pipeline standardization
   - Keep `preview_audio_tool` and `preview_overlay` as transient for "preview".
   - Apply writes to `edited_cache` and marks list (●).
2) Markers
   - Maintain `markers_pending` (preview), `markers_applied` (committed), `markers_saved` (disk).
   - UI status column: preview/pending, applied, saved.
3) Loop/Repeat
   - Separate preview loop selection vs applied loop edit.
   - Apply commits to `edited_cache` only.
4) Reset values on Apply
   - After Apply, reset tool state to defaults (fade durations, pitch, stretch, gain, normalize target, etc).

### Acceptance
- Preview stays until Apply; Apply keeps list ● without writing file.
- Save writes to disk and clears ●.
- Marker table shows correct state per row.

## Phase 6: Clipboard Reliability
### Requirements
- Ctrl+C / Ctrl+V always works (no intermittent misses).

### Implementation Plan
1) Centralize hotkey handling and add debug telemetry for focus and events.
2) Make list copy/paste trigger if either list has focus or no text field is focused.
3) Add fallback to OS clipboard if internal payload absent.

### Acceptance
- Repeated copy/paste works even after mouse interactions and focus changes.

## Phase 7: Loudness Normalize (YouTube-style)
### Requirements
- Normalize to target LUFS-I (e.g., -14 LUFS), preserving dynamics.

### Implementation Plan
1) Implement offline LUFS-I measurement (reuse existing LUFS pipeline).
2) Compute gain to target and apply as non-destructive edit (in memory).
3) Add Inspector tool: target LUFS, optional true-peak limiter (future).

### Acceptance
- Normalize changes gain to target LUFS-I; no destructive write until Save.

## Phase 8: Sample Rate Converter
### Requirements
- List context menu: resample (non-destructive).
- Apply to memory only; save writes to disk.

### Implementation Plan
1) Add resample tool using existing signal processing (or new resampler).
2) Store as edited cache with updated sample rate.
3) Ensure playback uses resampled buffer.

### Acceptance
- Sample rate change reflected in metadata; no file overwrite until Save.

## Phase 9: Spectrogram Time Axis
### Requirements
- Time grid labels in Spec/Mel view.

### Implementation Plan
1) Draw time ticks using current view offset and zoom.
2) Reuse existing time formatting utilities.

### Acceptance
- Spec/Mel show time labels aligned with waveform.

## Phase 10: Session Naming + Save Semantics
### Requirements
- "Session" naming should clearly mean app-state restore (legacy "project" term should not imply full project management).
- Ctrl+S should not ambiguously imply audio overwrite or export.
- Separate "session/state" save from "audio export".

### Implementation Plan
1) Rename feature in UI
   - Use "Session" terminology: "Session Open/Save/Save As".
2) Extension
   - Use a distinct extension that implies state, not audio (use `.nwsess`).
3) Shortcut policy
   - Ctrl+S = Session Save (overwrite current session state).
   - Ctrl+Shift+S = Session Save As.
   - Export/Render actions are under Export menu (Ctrl+E optional).
4) Menu / header cleanup
   - Remove Command Palette menu entry.
   - Group File/Open/Session actions in File; Export retains audio outputs.

### Acceptance
- Users can clearly tell the difference between "state save" and "audio export".
- Ctrl+S never writes audio files directly.
## Risks / Notes
- Multi-source merge must avoid reallocation per frame (cache the merged view).
- Tool Apply reset affects user workflow; use explicit defaults per tool.
- LUFS normalization needs consistent RMS/LUFS pipeline; avoid per-frame recompute.

## Milestones
1) External data multi-source merge + UI (Phase 1)
2) Focus/IME + file association behavior fixes (Phase 2-3)
3) Apply semantics (Phase 5)
4) Zoom/Clipboard/Spec time axis (Phase 4, 6, 9)
5) Normalize + SR convert (Phase 7-8)





