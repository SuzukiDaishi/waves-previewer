# Changelog

All notable changes in this repository (hand-written).

## Unreleased (current)

- Refactor into modules: `audio`, `wave`, `app`, minimal `main`.
- Add seamless loop playback (no gap) with loop toggle (button + `L`).
- Replace global volume with dB slider (-80..+6 dB), internally converted to linear gain.
- Smooth playhead updates using `request_repaint_after(16ms)`.
- Add Mode dropdown: Speed / PitchShift / TimeStretch.
  - Speed: realtime playback-rate change (0.25–4.0), pitch not preserved.
  - PitchShift: semitone shift (-12..+12), duration preserved, offline using `signalsmith-stretch`.
  - TimeStretch: stretch factor (0.25–4.0), pitch preserved, offline using `signalsmith-stretch`.
- Heavy processing system:
  - Pitch/Stretch run on a background thread; UI shows a full-screen blocking overlay with spinner and message until completion.
  - Results (processed buffer + waveform) are applied atomically on completion.
- Stretch/pitch tail handling:
  - Consider `output_latency()` and append `flush()` tail to avoid truncated endings; reduce loop boundary hiccups.
- List/UX tweaks:
  - Level (dBFS) palette expanded (black→deep blue→blue→cyan/green→yellow→orange→red) for clearer differences.
  - File name click opens editor tab (row also becomes selected). Background click continues to select + preload audio.
  - Folder cell click opens the folder in the OS file browser.
  - Disabled global hover brightening to avoid sluggish hover-follow effect; clickable cells now use button styling with pointer cursor.
  - Switching tabs now reloads the active tab's audio and loop state so playback always reflects the selected editor.
  - Columns added: Ch/SR/Bits; LUFS removed for performance (RMS dBFS kept). Sorting by any column via header click.
  - Added Search bar (filters by filename/folder), tri-state sorting (asc/desc/original), and auto-scroll to keep the selected row visible.
  - Top bar shows file counts (visible/total) with loading indicator (⏳) while metadata is still arriving.
  - Speed control moved to input field: "Speed x [1.0]" (0.25–4.0) with validation; audio engine supports fractional-rate playback with linear interpolation.
- List view rework/perf:
  - Use `TableBuilder` with internal vscroll and `min_scrolled_height(...)` to fill to bottom.
  - Virtualized rows via `TableBody::rows` (render only visible rows) for 10k–30k entries.
  - Whole-row click selection by setting `.sense(Sense::click())` and using `row.response()`.
  - Resizable columns; Wave column expands thumbnails (height tracks width).
  - Per-row background color for Level (dBFS) with overlaid text.
  - Async metadata worker (RMS + 128-bin thumbnails) with incremental updates.
  - Keyboard: Up/Down selection, Enter to open, click loads audio, double-click opens tab.
- Editor view improvements:
  - Waveform height grows with width; grid lines; amplitude-based coloring (blue→red).
- Fonts: Load Meiryo/Yu Gothic/MS Gothic on Windows to avoid tofu.
- Build notes: On Windows, install LLVM and set `LIBCLANG_PATH` when enabling `signalsmith-stretch`.
- Known issues documented (Windows EXE lock, UTF-8, etc.).

## 0.1.0 (initial)

- Basic egui app with WAV decoding (hound), CPAL output, min/max waveform, RMS meter.
