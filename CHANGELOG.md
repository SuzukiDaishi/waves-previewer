# Changelog

All notable changes in this repository (hand-written).

## Unreleased (current)

### UI Improvements (Latest)
- Split `src/app.rs` into submodules: `src/app/{types,helpers,meta,logic}.rs` for clearer responsibilities.
- Documented upcoming Editor 2.0 spec (multichannel lanes, dB grid, mouse seek, time zoom). See `docs/EDITOR_SPEC.md`.
- "Choose" メニューを2項目に整理し操作を明確化:
  - "Folder...": フォルダを選択して一覧を置き換え（rootに設定して再走査）
  - "Files...": 複数ファイルを選択して一覧を置き換え（rootはクリア）
- ドラッグ&ドロップでファイル/フォルダ追加に対応（WAVのみ、重複は自動スキップ）。追加時は検索/ソートを保ちつつメタを非同期再計算。
- 縦スクロールバーを常に右端に配置: テーブルに非表示の余白列（Column::remainder）を追加して右端まで広げるように変更。Wave列の表示位置は従来どおり維持。
- **Enhanced Keyboard Controls**: Added more intuitive keyboard shortcuts
  - **Ctrl+W**: Close active editor tab (with automatic audio stop)
  - Maintains existing shortcuts (Space for play/pause, L for loop toggle, arrow keys for navigation)
- **Improved Mouse Interaction**: Better click and double-click behavior
  - **Single-click**: All text columns (File/Folder/Length/Ch/SR/Bits/Level/Wave) now selectable for easier navigation
  - **Double-click on File name**: Opens file in editor tab (was single-click before)
  - **Double-click on Folder**: Opens folder in system file browser with the WAV file pre-selected
  - **Single-click on row background**: Selects row and loads audio (unchanged)
- **Tab Navigation Audio Control**: Enhanced audio control for better user experience
  - Switching between tabs (List ⇔ Editor) now automatically stops audio playback
  - Closing editor tabs with the "x" button also stops audio playback
  - Prevents confusion from audio continuing when user switches context
- **Playback Behavior**: List view now always disables loop playback for better audio previewing
  - List display: Always plays once and stops (optimal for quick audio preview)
  - Editor tabs: Loop toggle available via L key (for detailed editing work)
- **Table Layout Fixes**: Fixed text overflow and header collision issues
  - Added Length column (mm:ss format) with proper sorting by duration in seconds
  - Made all columns resizable with optimized initial widths
  - Long text (file names, folder paths) now truncates with "..." and shows full text on hover
  - Improved cell layout to prevent text from appearing behind headers

### Editor View
- Implemented mouse seek/scrub and time zoom/pan interactions
  - Click/drag to seek; Ctrl+Wheel to zoom; Shift+Wheel (or horizontal wheel) to pan.

### Core Features
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
- Docs: Added editing roadmap (planned) to README/UX/EDITOR_SPEC
