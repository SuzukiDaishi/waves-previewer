# Changelog

All notable changes in this repository (hand-written).

## Unreleased (current)

### RX-Style Time-Frequency Selection: Spectral Mute + Play Selection
- The spectral views (Spec / Freq Log / Mel) now support time-frequency rectangle selection: dragging selects both the time range and a frequency band (drawn as a band-limited highlight per channel lane), like iZotope RX / Adobe Audition's marquee. Dragging edge-to-edge across the whole frequency axis - or dragging in the Wave view - keeps the classic full-band time selection. The Y->Hz mapping follows the active view's axis exactly (linear / log / mel, including vertical zoom and the display max-frequency cap), and the band survives undo/redo.
- New "Freq" row in the Inspector shows the selected band with editable low/high Hz fields and a "Full band" reset; it appears in the spectral views (and anywhere a band is set).
- "Mute Selection (Spectral)": destructively mutes only the selected band inside the selected time range. The band is removed with an STFT band-stop (Hann, 75% overlap, weighted overlap-add resynthesis) with raised-cosine transition bands at the frequency edges, and the filtered result is crossfaded against the original with raised-cosine time fades just inside the selection - no clicks, no brick-wall ringing (the same edge-smoothing approach RX and Audition use). Without a band it is a click-free full-band mute. Fully undoable; edge fade lengths (ms / Hz) are adjustable in the Inspector.
- "Play Selection" (Wave view too): plays only the selected time range, band-passed to the selected frequency band when one is set (RX-style selection audition). Follows the offline-render playback principle (never filters in the audio callback), auto-stops at the selection end, loops the selection while loop mode is on, and restores the tab's real audio when playback stops. Band-pass/band-stop DSP is covered by unit tests (band isolation, STFT round-trip transparency, click-free edge ramps).

### Harvest F0 Estimator (switchable) + Resynthesis Quality Audit
- New "F0 estimator" setting in the World inspector: `DIO (fast)` (default, unchanged) or `Harvest (accurate)` - a full pure-Rust port of WORLD's Harvest (filter-bank candidate detection on a 1 ms grid, instantaneous-frequency refinement, unreliable-candidate removal, contour fixing, zero-lag Butterworth smoothing). Harvest replaces DIO+StoneMask before CheapTrick/D4C and also drives resynthesis re-analysis. Cross-validated against pyworld 0.3.5: 100% voiced/unvoiced agreement and 0.06-cent median F0 difference on a vibrato test tone. The refinement stage (the heavy part) fans out across worker threads, and progress reporting stays live. Persisted in prefs; switching estimators drops cached World analyses so views re-analyze.
- Audited F0-edit -> resynthesize quality against the reference implementation: no beyond-spec defects found. Sample rate is guarded end-to-end (spawn-time mismatch check; pitch roundtrips exactly at 44.1 kHz and 48 kHz), and long-clip smearing was already fixed by the 5 ms fine re-analysis. The one surprise - pure sine tones come back ~+4 dB hot - reproduces bit-for-bit in the reference vocoder (pyworld measures the same +3.98 dB; harmonic-rich material roundtrips at ~+0.2 dB), so it is inherent CheapTrick envelope behavior, not a port bug. New regression tests pin all three facts (harmonic roundtrip within 1.5 dB, sine matches the reference gain, flat-envelope synthesis calibration).

### Background Session Save / Clipboard Copy + Cheaper Undo & Edits
- Session save no longer freezes the UI: the document and Arc snapshots of every edited/virtual audio buffer are gathered instantly, then all sidecar WAV encodes, TOML serialization, and file writes run on a worker while the busy overlay shows progress ("Saving session... (N audio sidecars)"). Close-with-autosave, CLI, and tests keep a synchronous variant so completion stays observable where it matters.
- Copying items to the clipboard is backgrounded the same way: decoding file-backed items and exporting edited audio to temp WAVs happen on a worker; the OS clipboard and in-app payload are set on completion. Large multi-selections no longer lock the app for seconds.
- Undo snapshots are Arc-shared with the tab's worker mirror: capturing an undo point before an edit is now copy-free (was a full multi-MB buffer clone per edit), and undo/redo drop from three full-buffer copies to one plus the engine hand-off.
- In-place destructive edits (trim / fade / gain / delete / reverse...) defer the waveform overview + pyramid rebuild to a background worker with generation guarding; the edit itself lands immediately and the refreshed overview swaps in when ready instead of stalling the frame for the rebuild.

### List & Apply-Path Performance Pass (large libraries, long clips)
- Removed two full item-array scans that ran every frame (pending-gain count in the topbar and the list-header dirty check): both now read a 250 ms-throttled cached count that gain edits invalidate immediately. At ~140k files these two scans strided tens of MB of item structs per frame - the main reason the list felt heavy while background jobs forced 60 fps repaints. Idle frame time on a 140k-row list drops ~4.1 ms -> ~1.9 ms avg.
- Meta-driven re-sorts are debounced adaptively: lists over 20k items re-sort at most every 750 ms while metadata streams in (was 120 ms; each pass is an O(n log n) decorate+sort costing tens of ms at 140k). Transcript-triggered search refilters follow the same adaptive debounce.
- List rows no longer deep-clone the whole MediaItem (strings, external map, inline FileMeta with thumbnail) for every visible row every frame; rows now borrow the item once and keep only the cheap pieces (badge, cover-art Arc, transcript Arc).
- Pitch/stretch/loudness applies and WORLD resynthesis now build the waveform overview + pyramid and the worker-facing Arc mirror on the worker thread; adopting a finished apply no longer re-scans and re-clones the full buffers on the UI thread (~35-80 ms saved per apply on a 3-minute stereo clip).
- Rate-mode processing results (Speed/Pitch/Stretch previews) also prebuild the editor waveform cache on the worker, and two wasteful full-buffer clones in the completion handler were removed (the engine now takes the processed buffers by move).

### Spectrogram Display Fixes: Stale Partial Render + Resolution
- Fixed the spectrogram (and Freq Log / Mel) showing a partially-filled image with a black tail when a view is opened while analysis tiles are still streaming in - the first render stuck around until a zoom/pan happened to change the render key. Tile arrival and completion now retire the cached viewport image, so the heatmap fills in progressively and always ends complete.
- Feature-view render resolution unlocked: the fine pass now renders at native pixel size (up to 2048x1024; previously hard-capped at 384x192 and stretched), so Spec/Freq Log/Mel/Tempogram/Chromagram/World are sharp on large canvases. The coarse preview pass got a matching bump.

### WORLD Responsiveness / Undo Correctness Pass
- Fixed Ctrl+Z after destructive edits (including WORLD resynthesis): undo/redo now refreshes the worker-facing buffer mirror and drops stale spectrogram/feature analyses, so the World view (and Spec/Tempo/Chroma) re-analyze the audio that is actually restored instead of showing the pre-undo analysis.
- WORLD analysis now reports live progress (DIO -> StoneMask -> CheapTrick -> D4C weighted 0-100%): the inspector progress bar animates, the canvas overlay shows a percentage, and the frame loop keeps ticking during analysis so feedback never freezes.
- Removed every UI-thread stall in the World pipeline: analysis mixdown moved onto the worker thread (applies to Tempogram/Chromagram too), viewport render requests share the cached analysis via Arc instead of deep-cloning tens of MB per pan/zoom, and the envelope maximum is precomputed at analysis time instead of rescanned on every render.
- F0 curve drawing is decimated to ~2 points per pixel (window-aware so unvoiced gaps still break the line), keeping long clips smooth while editing.
- Dev builds (`cargo run`) now compile at opt-level 1 with hot DSP crates at full optimization - the WORLD/FFT paths were 10-20x slower unoptimized, which made debug builds feel hung; lib test wall time dropped from ~20 s to ~2 s as a side effect.

### F0 Editing + WORLD Resynthesis
- The World view is now an editor: enable "Edit F0 on canvas" and draw the pitch curve with the mouse (left-drag draws, right-drag erases to unvoiced; strokes interpolate in log-frequency so fast drags leave no gaps). Canvas seek/select pause while editing.
- Curve transforms in the inspector: semitone shift (drag value + apply), 5-frame median smooth, flatten-to-median (monotone), and reset to the analyzed curve. The edited draft renders in orange over the dimmed analyzed curve.
- "Resynthesize (replace audio)" rebuilds the tab audio with WORLD synthesis using the edited F0 - ported D4C aperiodicity analysis and the reference synthesis engine (pulse/noise excitation through minimum-phase spectra, fractional pulse alignment, deterministic noise) join the analysis port in `render/world_features.rs`. Runs as a background job through the shared editor-apply pipeline: full undo (Ctrl+Z), busy overlay with cancel, engine buffer swap, and cache invalidation; the mono result is written to every channel so the tab keeps its channel count. Roundtrip unit tests confirm pitch is preserved and that editing the contour actually shifts the resynthesized pitch (1 s of 48 kHz synthesizes in ~30 ms release).
- F0 readability: pitch curves now draw over a dark halo so they stay visible on bright envelope areas, and a new "F0 zoom" toggle switches the vertical axis to 50 Hz-1.1 kHz so the pitch range fills the canvas (heatmap, ticks, and pencil mapping all follow).

### Spectrogram dB Reference Option
- New "Spectrogram Values" setting: `dB (0 dBFS ref)` (previous behavior) or `dB (normalized to max)` - librosa-style `ref=max` mapping where the loudest bin tops the color ramp, keeping harmonic detail visible on quiet material. Persisted in prefs and sessions; applies to Spec/Freq Log/Mel views.

### New WORLD Feature View (F0 / Spectral Envelope)
- New editor view "World (F0/Env)" alongside Tempogram/Chromagram: a CheapTrick spectral-envelope heatmap on a log-frequency axis with the DIO+StoneMask F0 trajectory overlaid as a cyan polyline, a live F0 readout at the playhead, and frequency-axis ticks in the gutter.
- The analysis is an independent pure-Rust port of the WORLD vocoder's core algorithms (mmorise/World, BSD-3-Clause) in `render/world_features.rs` — DIO band-wise zero-crossing F0 candidates, StoneMask instantaneous-frequency refinement, CheapTrick pitch-adaptive envelope — with unit tests covering sines (55/100/440 Hz), sweeps, silence/noise voicing decisions, and envelope peak placement.
- Runs as a cached background job like the other feature views (auto-starts on view switch, cancel/progress wired, invalidated on edits); frame period scales with clip length so long files stay bounded. Inspector shows median F0, voiced ratio, hop size, and a Re-analyze button.
- Wired through session persistence (`other_view: "world"`, legacy `"f0"` accepted), the `S` view-cycle hotkey, `--open-view-mode world`, export-settings view picker, and kittest coverage (view switching + an end-to-end analysis test).

### MiniMeter Overhaul: Vectorscope, Per-Channel Peaks, Better Analyzer
- New STEREO panel in the editor bottom strip: goniometer/vectorscope (Lissajous, auto-gain, L/R diagonal guides) plus a smoothed correlation bar (-1..+1). Mono files collapse onto the mid axis and show a MONO badge; files with 3+ channels visualize the first pair and show a CH1+2 badge.
- PEAK panel now draws one bar per channel for any channel count (L/R labels for stereo, numbered otherwise), each with its own peak-hold and RMS tick; the readout shows the loudest channel.
- Spectrum analyzer is dual-resolution: a long FFT (~170 ms window) feeds the low band so bass peaks are localized instead of smeared, a short FFT keeps the high band fast, with a log-domain blend across the crossover; sub-bin columns are interpolated so lows render as a smooth curve.
- Analyzer ballistics: fast attack (~10 ms) with a prompt release (~100 ms) so bars fall cleanly back to the floor when the signal goes quiet, and the strip keeps animating until the decay settles after playback stops.
- Meter DSP moved to `render/mini_meter.rs` with unit tests (low/mid/high peak accuracy, dBFS calibration, ballistics, correlation) and a frame-budget test; per-frame state lives on the tab (no per-frame allocations), keeping the strip comfortably inside a 30 fps budget.
- Fixed Linux link failure of the `neowaves` binary: the DirectML execution provider was referenced unconditionally in transcription session setup (Windows-only symbol).

## 0.20260704.0 - 2026-07-04

### UI Overhaul: Effect Graph, Resizable Panels, Seam Check, MiniMeter (Latest)
- Effect Graph console now docks under the canvas only (left palette stays full height), with a drag-resizable, height-clamped panel so it can no longer swallow half the window; rows are monospace, severity-colored, truncated with tooltips, and the header shows a validation-issue count.
- Effect Graph nodes restyled: soft drop shadow, accent-tinted header with underline, slimmer status border with a selection glow, pill-shaped elapsed-time badge, ringed port pins, and cables with a dark underlay for depth. Left/right panels gained sane min/max resize ranges.
- Editor inspector width is drag-resizable via a divider between canvas and inspector (remembered for the session); Effect Graph side panels are also clamped-resizable.
- Loop Inspector replaced with a real seam-continuity check: the audio running into the loop end and out of the loop start is drawn as one continuous trace joined at the jump, with the crossfaded result overlaid, a log-scale window zoom (2–250 ms), auto-gain, and a click-risk verdict (amplitude step vs. local motion).
- New MiniMeter strip fills the empty space under the editor overview: realtime oscilloscope, log-frequency spectrum analyzer with hue-swept bars, and a peak/RMS meter with peak-hold, all following the playhead.

### Inspector Overhaul: Loop Edit / Auto Trim / Tempogram / Chromagram
- Loop Edit no longer overflows the inspector: a mis-nested layout row swallowed the Apply button and the whole Auto Detect section into one wrapping row; crossfade controls now sit on two rows, loop-range readouts truncate with tooltips, and detect candidates render as fixed-width color-coded rows.
- Loop auto-detect scoring got stricter and more musical: anti-correlated seams no longer earn a baseline score, a long-range loudness-envelope similarity term rewards structurally matching sections, near-silent seams are penalized, and refined candidates deduplicate within ~20 ms so the list shows distinct alternatives.
- Auto Trim is now live: thresholds are sliders with units and plain-language tooltips, the measured noise floor / peak / effective threshold are shown in dB after a run, and edits re-run detection automatically (debounced) so the selected ranges update as you drag.
- Tempogram is readable: values are normalized globally (silence stays dark instead of amplifying noise), the BPM axis is always drawn, and a green guide line + label marks the estimated BPM with half/double-tempo hints in the panel.
- Chromagram is readable: displayed values use per-frame raw chroma (key estimation still runs on the CENS profile), pitch-class bands are equal-height and aligned with always-visible note labels, and the detected key's row is highlighted.
- Inspector styling pass: consistent accent-bar section headers, unified spacing, confidence meters for BPM/key estimates.

### FLAC Support + Format/Metadata Matrix
- Added FLAC decode via symphonia (`flac` feature) and FLAC encode via `flacenc` (16/24-bit; 32-bit float sources are quantized to 24-bit since FLAC has no float representation).
- FLAC now works across list/editor load, save/overwrite, format convert ("To FLAC"), gain export, and virtual-item export; list shows a FLAC badge.
- Loop markers for FLAC are stored as Vorbis comments (`LOOPSTART`/`LOOPEND`, same convention as MP3/M4A); BPM (`BPM`/`TEMPO` comment) and cover art (`PICTURE` block) are read.
- FLAC→FLAC saves carry `VORBIS_COMMENT` + `PICTURE` blocks over (stream-dependent `SEEKTABLE`/`CUESHEET` are intentionally dropped).
- OGG loop markers no longer fail the whole save: formats without in-file loop support now fall back to a `<stem>.loop.json` sidecar (read + write).
- Installer: added missing `.aiff`/`.aif`/`.ogg` file associations and new `.flac`.
- Documented the per-format support matrix and export policy for unsupported metadata in `docs/FORMAT_SUPPORT.md`; updated README format list.

### CLI Replacement / MCP Removal
- Added docs-first CLI replacement specs under `docs/CLI_*.md`.
- Default startup remains GUI; headless automation now enters through `--cli`.
- Replaced the handwritten startup parser with `clap`, including richer `--help` output for GUI mode and CLI subcommands.
- Added Phase 1 headless commands for session/item/list/editor/render/export/debug with JSON stdout envelopes.
- Added direct waveform/spectrum PNG rendering and GUI-backed list/editor screenshot rendering for CLI workflows.
- Removed runtime MCP wiring from the app shell and menus; repo/docs now point to `--cli` as the supported automation surface.

### Refactor: Large File / Large Function Split
- Split app startup and frame orchestration out of `src/app.rs` into `src/app/app_init.rs` and `src/app/frame_ops.rs`.
- Moved tab open/activate and editor decode orchestration into `src/app/tab_ops.rs` and `src/app/editor_decode_ops.rs`.
- Split top bar UI into `src/app/ui/topbar/{menus,transport,status}.rs` and reduced the large status-row renderer into smaller activity helpers.
- Split CLI parsing out of `src/main.rs` into `src/cli.rs`, keeping `main.rs` focused on native startup.
- Split list UI support code into `src/app/ui/list/navigation.rs` and `src/app/ui/list/table.rs`; `ui_list_view` now acts as the main orchestration entry instead of carrying focus logic and table definition inline.
- Documented current staged large-file exceptions in `README.md` and `AGENTS.md` so remaining big files are explicit rather than implicit.

### Settings/Theme + Undo/Redo + List UX (Latest)
- Added Appearance setting (Dark/Light), default Dark; preference persists across restarts.
- Fixed initial theme application so startup respects the saved theme.
- Added editor Undo/Redo (Ctrl+Z / Ctrl+Shift+Z) with toolbar buttons; destructive ops are tracked.
- List UX: click selection no longer auto-centers; keyboard selection still auto-centers.
- Metadata loading now prioritizes visible rows when jump-scrolling.

### Waveform/Overlay Consistency + Loop UI Simplification (Latest)
- Overlay rendering reworked to match base waveform across all zoom modes.
  - Line (spp < 1.0): per-sample polyline + stems (pps >= 6) — identical to base.
  - Aggregated (spp >= 1.0): pixel-locked min/max bins per px column — identical to base.
  - Time-stretched overlays map visible window via ratio; binning uses base px columns to avoid drift.
  - LoopEdit boundaries are emphasized by drawing the same bins again with a thicker stroke.
  - Fixed overlay-window mapping: start/end now derived from the visible window, not the whole file.
- Loop controls in the top bar are simplified: keep only Loop mode toggles (Off / On / Marker).
  - Numeric seconds for Start/End and Set Start/End/Clear were removed from the top bar.
  - Loop region editing is now centralized in Inspector > LoopEdit (samples), K/P keys still supported.
- Added debug prints for zoom/overlay mapping in dev builds to diagnose platform-specific input/rounding.

### Editor Loop/Selection Rework (Breaking)
- Removed range Selection and the Seek/Select tool. The canvas always seeks on click.
- Introduced independent `loop_region` per editor tab. Loop playback uses:
  - `Off` / `OnWhole` / `Marker` (Marker uses `loop_region`), toggled via `L`.
  - Start/End can be edited as samples in Inspector > LoopEdit.
  - (Changed) The top bar no longer offers numeric Start/End editing.
  - Added buttons to set Start/End from current playhead position.
  - New: Loop crossfade. Configure duration (ms) and shape (Linear/EqualPower) in
    LoopEdit. Playback blends end→start inside the last N samples for click‑free loops.
- WAV `smpl` loop markers are now read on load and mapped into `loop_region` (SR conversion considered).
- Inspector changes:
  - LoopEdit shows Start/End (samples), Set Start/End @ Playhead, Clear Loop.
  - Trim/Fade/Gain/Normalize/Reverse/Silence now apply to Whole only.
  - Export Selection removed.
- Keyboard changes:
  - K = Set Loop Start @ playhead, P = Set Loop End @ playhead
  - L = Loop Off ⇄ OnWhole toggle
  - Removed A/B and I/O bindings (Selection removed)
- Fixed pending action wiring: Reverse/Gain/Normalize/Silence are now correctly applied and update playback/loop state.
- Play position can be edited numerically (seconds) from the top bar.

### UI Improvements (Latest)
- Editor zoom/pan reliability: fixed cases where Ctrl+Wheel zoom didn't fire on some environments.
  - Hover detection now uses canvas-rect hit test instead of `Response::hovered`.
  - Wheel input combines `raw_scroll_delta` with low-level `Event::Scroll` and pinch `Event::Zoom`.
  - Added optional debug trace in dev builds to log incoming deltas.
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
  - Columns added: Ch/SR/Bits に加えて LUFS (I) と Gain (dB) を表示。LUFS は近似→非同期再計算で更新し、すべての列で tri-state ソートに対応。
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
- Dependency bumps (compat)
  - cpal: 0.15 → 0.16 (no code changes required here)
  - rfd: 0.14 → 0.15.4
  - egui/eframe/egui_extras remain at 0.27 series intentionally for now to avoid
    a large breaking migration to 0.32+. We will plan that upgrade separately.
