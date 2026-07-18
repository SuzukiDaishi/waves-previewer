# Changelog

All notable changes in this repository (hand-written).

## Unreleased (current)

### Playback & Metering (P6)
- **Realtime LUFS + true peak**: the audio callback feeds a lock-free tap ring; a low-priority thread runs BS.1770 K-weighting (recomputed for the device sample rate and pinned to the ITU 48 kHz table by test), publishes momentary (400 ms) / short-term (3 s) LUFS and 4x-oversampled true peak, shown as a compact "M / S / TP" readout next to the topbar output meter. Readings invalidate ~500 ms after playback stops.
- **Goniometer polish**: the STEREO pane's Lissajous mapping is now a unit-tested pure function (mono collapses to the mid axis, L = -R to the side axis) and the smoothed correlation value is shown numerically beside the pane title.
- **Play Selected Together** (List menu): decode up to 16 selected files, align sample rates, mix at 1/sqrt(n), and play the sum once — a quick layering check without leaving the list.
- **Resampler quality**: the Pitch/Time-Stretch offline pre-stage and lossy-encode paths now use rubato sinc SRC (Good), and the LUFS 48 kHz conversion uses Fast sinc instead of linear interpolation (loudness goldens unchanged). Multichannel LUFS weighting (5.1/7.1 surround x1.41, LFE excluded) confirmed shipped and its stale docs corrected.

### DSP & RX Parity (P5)
- **Noise-shaped dither + 24-bit**: export dither is now a mode (Off / TPDF / TPDF + noise shaping); noise shaping adds per-channel 2nd-order error-feedback (NTF = (1 - z^-1)^2) pushing quantization noise out of the most audible band. A unified Quantizer backs all PCM paths (WAV/AIFF/converter/FLAC two-pass, determinism preserved), and 24-bit exports can opt into dithering. Prefs migrate from the old boolean key.
- **De-clip tool**: detects flat runs pinned at the clipping rails (peak-relative threshold + corner test that rejects smooth low-frequency crests, square-wave rails rejected by run length) and rebuilds the chopped crests with the de-click Hermite bridge — the repair can rise above the rail (float headroom preserved). Scan overlay + async Apply + CLI support.
- **De-hum tool**: cascade of narrow RBJ biquad cuts at the mains fundamental and up to 16 harmonics (STFT rejected: 2048-bin resolution is too coarse for 50/60 Hz). Detect sweeps 45-65 Hz with Goertzel probes; Hz/harmonics/Q/depth adjustable; a selection limits the apply via crossfaded splice. CLI supported.
- **Edit history panel** (Edit > History...): labeled undo/redo entries (operation names from the concrete apply paths), click to jump multiple steps through the existing undo/redo machinery.
- **Region list** (Edit > Regions...): labeled ranges on the editor tab that ride undo and destructive-edit remapping like markers; add-from-selection, inline rename, click-to-select, sidecar (<file>.regions.json) + .nwsess persistence, CSV export.
- **Scrub playback**: Alt+drag on the waveform loops a ±40 ms window under the pointer via the existing loop atomics; release restores the previous loop/transport state exactly.
- **WORLD aperiodicity editing**: per-frame breathiness multiplier draft (Set All / Set Selection / Reset) baked in at Resynthesize, clamped into 0..1 per band; fine 5 ms re-analysis resamples the curve.
- **Spectral region copy/paste**: with a frequency selection in Spec/Log views, Ctrl+C copies band-masked STFT frames and Ctrl+V replaces (Ctrl+Shift+V adds) the band content at the selection start/playhead, snapped to the hop grid, same-sample-rate only.
- **Harmonic action**: Ctrl+click a partial in Spec/Log — f0 refines onto the nearest peak, harmonic bands highlight, and one multi-band STFT pass mutes or attenuates the whole stack over the selection.

### Usability Completion (P4)
- **Non-blocking heavy applies**: pitch/stretch/speed/loudness, de-click/de-noise, spectral warp/brush/heal, and WORLD resynthesis no longer raise the app-wide modal overlay. Only the target tab is gated (in-tab banner); the list, other tabs, and playback of other sources stay interactive. Progress + Cancel live in the topbar activity slot. Tabs are tracked by a stable id, so closing a tab mid-apply discards the result instead of corrupting whichever tab shifted into its index. One apply runs at a time.
- **Rebindable shortcuts**: Help > Customize Shortcuts... lets table-dispatched chords be reassigned by clicking a row and pressing the new chord (conflicts across overlapping contexts refused, per-row Reset / Reset All, persisted as `keymap=` prefs lines). The read-only shortcut list shows the effective (overridden) chords.
- **Tool icon toolbar**: the editor's 22-item Tool ComboBox is now a grouped icon toolbar (hover for names, wraps in narrow panels) with the active tool highlighted; selection semantics (preview discard, gesture reset) unchanged.
- **Editor zoom/nav keys**: `+`/`=` zoom in, `-` zooms out around the playhead; `[`/`]` page the view by one visible width.
- **Wheel behavior option**: Settings > "Wheel scrolls the view (Ctrl+wheel zooms)" turns a plain vertical wheel into horizontal view scrolling (Ctrl+wheel / pinch still zooms). Default stays zoom-on-wheel.
- **Edit menu** (File | Edit | Export) with Undo/Redo wired to the same dispatch as `Ctrl+Z`/`Ctrl+Y`, enabled from the editor/list/effect-graph undo stacks.
- **List context menu**: Open in Editor and Reveal in Folder at the top; Select All / Clear Selection at the bottom. Right-clicking inside a multi-selection keeps it.
- **Empty-state onboarding**: with no folder and no items, the list shows a centered panel with Open Folder... and up to five recent sessions.
- **Polarity invert boundary smoothing** (option, default off): ~2 ms polarity crossfade at interior range boundaries so partial inverts don't click; edge-touching ranges and the default path stay bit-exact.

### Pipeline & QA (Stage B / P3)
- **Naming-rule check** in batch inspection (GUI dialog + CLI `--naming-pattern`): file stems failing the regex get warnings; an invalid pattern reports a config error on every row. Pattern persists to prefs.
- **Find Duplicates** (List menu): worker-pool fingerprinting (gain-invariant spectral-shape hashes + exact content hash) clusters exact duplicates and perceptually similar files into a results window with click-to-select and CSV export.
- **Export Engine Metadata** (List menu + CLI `batch engine-export`): Unity JSON / FMOD JSON / Wwise TSV metadata tables (loops, sample rate, channels, length, LUFS) for the selection or list — no audio conversion.
- **Edit BWF Metadata** (List menu): batch-write the bext chunk (description/originator/reference, auto-stamped date/time) into selected WAVs, preserving all other chunks; non-WAV files are skipped and counted. iXML remains out of scope.
- **WORLD formant editing**: a Formant slider (0.5x-2.0x) in the World view warps the spectral envelope along frequency at resynthesis — formant shifts without pitch changes, applied in both the display-grid and fine 5 ms re-analysis paths.
- **Light theme pass**: hand-painted widgets (list selection/markers, dirty/error accents, volume slider, output meter) now draw through a theme-aware palette; the editor's audio canvas intentionally stays dark (DAW-style) in both themes.

### Spectral Repair & Restoration (Stage A)
- **Spectral Brush** (Spec/Log views, next to Spectral Warp): drag on the spectrogram to paint content out RX-eraser-style. Stamps attenuate magnitude with Gaussian falloff in time and frequency (Strength 3-80 dB, Radius ms/Hz baked per stamp), stack additively in dB (clamped at 80 dB), render a preview on release, and Apply through the async pipeline with undo. Only the influenced region is processed; audio outside the stroke stays bit-identical.
- **Heal Selection** (beside the spectral Mute button): rebuilds the selected time range (optionally band-limited by a frequency selection) from the surrounding audio — per-bin magnitudes interpolate across the gap between the context averages and phase advances at the measured per-bin velocity, so steady tones bridge dropouts coherently. Selections over 120 s are refused with a toast.
- **De-click tool**: second-difference residual detection with per-window MAD-adaptive threshold (sensitivity slider), Hermite-bridge repair. Scan marks the detected spans in red on the waveform (invalidated by any edit or sensitivity change); Apply repairs whole file or selection with undo. Also available via CLI `apply`.
- **De-noise tool**: learn a per-channel noise profile from a noise-only selection, then reduce it via power spectral subtraction (Reduction = max attenuation floor, Strength = over-subtraction) with asymmetric gain smoothing against musical noise. Preview/Apply through the shared worker pipelines; selection-scoped applies crossfade their edges.
- Shared STFT engine refactor: `stft_process_frames` (reflect-padded Hann WOLA, 2048/512) now backs the band gain, brush, heal, and noise-profile paths.

### Waveform Editing Completion (Stage A)
- **Mix paste** (`Ctrl+Shift+V`) sums the clipboard into the buffer without changing length; **crossfade-insert paste** (`Ctrl+Alt+V`) splices the clip in with equal-power joins at both seams.
- **Pencil tool**: at high zoom (> 2 px per sample) drag on the waveform to draw sample values directly (linear interpolation between drag points, lane-targeted, one undo step per stroke).
- **Channel-scoped edits**: with a Custom channel view active, gain / normalize / fade / mute / noise gate / EQ / compressor / DC removal / polarity invert apply only to the visible channels (normalize measures its peak within them). Light previews follow the same mask and the inspector shows "Applies to: ch N". File-level list gain deliberately ignores the mask.
- `EditorTab` construction deduplicated into `EditorTab::new_base` (one place to default new fields).

### Plugins (Stage A)
- **Presets & A/B**: save/load/delete named parameter presets (JSON per plugin under the NeoWaves config dir, state blob included) from both the effect-graph plugin node and the editor's Plugin FX tool; an A/B slot stores a second parameter set and swaps on demand.
- **Plugin Manager window** (Tools menu): catalog overview, rescan with status/error display, and search-path management persisted to prefs.
- **Auto preview**: a Plugin FX toggle re-renders the preview ~300 ms after any parameter change (sliders, Enable/Bypass, presets, A/B, native-GUI edits) with a position-preserving buffer swap, so tweaking parameters feels continuous.

### List (Stage A)
- **Multi-variation audition**: with 2+ rows selected, List > Audition Selection plays them in round-robin or random order (never the same file twice in a row), advancing on each natural playback end. Stop playback, select another row, or press Cancel on the topbar "Audition n/m" item to end it.

### Batch QA (P2 batch)
- **Inspect Files (QA)**: batch inspection over the selection or the whole list — effective true-peak ceiling, integrated-loudness window, leading/trailing silence thresholds, and loop-marker validity (bounds checking that the readers never did). Runs on up to four low-priority worker threads with topbar progress + cancel; results open in a severity-filtered window (click a row to select the file, Save CSV...). Same checks are exposed as `--cli batch inspect` with json/csv/md/txt reports.
- **Normalize Loudness (GUI)**: batch loudness normalize to a target LUFS (default -14) for the selection or whole list. Measures via the async metadata pool, then routes each file's gain delta through the unified gain framework — pending list gain (one undo action for the whole batch) or a destructive edit for files open in editor tabs. Non-destructive: no audio files are written; clip-risk files are counted and reported in the completion toast.

### Waveform Editing Basics (P2 batch)
- New editor tools: **Invert Polarity** (flip sample polarity over the selection or whole file) and **DC Offset** removal (per-channel mean subtraction with a live measured-DC readout), both with preview, undo, session restore, and CLI apply support.
- **Insert Silence** tool inserts N ms of zeros at the selection start (or the playhead); markers, loop regions, selections, and fade ranges after the insert point shift right. Built on a shared insert infrastructure (`editor_insert_channels_at`).
- **In-editor audio cut/copy/paste-insert**: Ctrl+C/X/V in the editor workspace operate on an in-app audio clipboard. Paste splices at the selection start / playhead with undo; cross-tab pastes are resampled to the target buffer rate and channel-adapted.
- **TPDF dither** (default on, Settings toggle) when quantizing to 16-bit integer PCM in the WAV/AIFF/FLAC/gain-export writers. Deterministic generator keeps FLAC's two-pass MD5 self-consistent.

### Usability (P1 batch)
- New Help menu with a read-only Keyboard Shortcuts window, generated from a central keymap table (`src/app/keymap.rs`); simple shortcut dispatch now goes through the table so a future rebinding UI only needs to swap the lookup.
- Destructive editor keys `C` (delete+join) and `T` (trim) show an info toast pointing at Ctrl+Z after they fire.
- Editor: `Home`/`End` seek to start/end, `Z` zooms to the selection, `Esc` discards a pending tool preview.
- Editor: per-channel playback mute/solo (M/S menu next to the channel view toggles). Monitoring only - the masks resolve to channel selection inside the callback's fold-down mapping, are excluded from undo/dirty/save, and never apply to list playback.
- Topbar output meter shows per-output-channel RMS bars with peak-hold ticks while the callback reports multichannel levels (falls back to the old single bar otherwise).
- List: optional "Single click auditions" setting (default on = current behavior). When off, a single click only selects; Space, keyboard navigation, and Auto Play still audition. Double-click still opens the editor.
- List: inline rename via `F2` (or the context menu) with Enter to commit and Esc to cancel; errors surface as toasts. The modal rename stays for batch use.
- List: column widths persist across sessions (saved when a resize drag ends; window-squeeze relayouts are never saved). Column reorder and per-project widths remain out of scope.

### Data Safety
- Windows file overwrite now uses `ReplaceFileW` (atomic swap), removing the crash window where the destination could be left missing during the park-and-rename fallback (which remains as last resort).
- Gain / Normalize / Loudness applies no longer hard-clip the editing buffer to +/-1.0; editing buffers keep full float headroom (boost then cut round-trips losslessly). Clipping only happens at export/quantize and playback output. An info toast reports when an edit leaves peaks above 0 dBFS.
- Closing the window with unsaved in-memory edits (dirty tabs, cached edits, pending gains) now asks for confirmation instead of silently discarding them; Ctrl+W on a dirty tab routes through the Leave Editor prompt. Screenshot/debug automation exits bypass the prompt.

### Notifications
- New toast overlay (below the topbar, click to dismiss, auto-expiring) surfaces failures that previously only reached the debug log or stderr: session save/save-as/close errors, export failures, editor tab-limit skips, and resampler quality fallbacks.

### Playback
- Sources with more channels than the output device are folded down (each output channel averages the source channels congruent to it) instead of dropping the surplus channels.
- Tool previews (Fade / Gain / Normalize / Loudness / Reverse / NoiseGate / EQ / Compressor / LoopEdit unwrap / MusicAnalyze) now play the per-channel buffer instead of a mono mixdown, preserving stereo imaging. Normalize previews measure peak across all channels, matching the destructive apply.

### Correctness
- Loop edits via the K/P shortcuts now push editor undo states (matching L / Inspector loop applies).
- Digit-key seek fixed: both `0` and `1` used to jump to the end. Keys `1..9,0` now span start (0%) to end (100%) in keyboard row order.
- 16-bit PCM encode/decode uses symmetric 32768 scaling (standard convention; -1.0 maps to -32768). The generic integer writer quantizes symmetrically for all depths.
- Spectral/feature lanes (Spec/Log/Mel/Tempogram/Chromagram/World) no longer drift up to one STFT hop against the waveform lane at high zoom (fractional per-column frame mapping).
- Meta pool and VST3 state-stream mutexes recover from poisoning instead of cascading panics; removed per-event wheel debug prints.

## 0.20260709.0 - 2026-07-09

### Unified Gain Framework: List Volume Changes Are Editor Edits
- Per-file volume changes made in the list (gain column DragValue, Left/Right arrow keys) and the Editor's Gain tool now live in one edit framework. When a file has an open, fully loaded editor tab, a list gain change is applied as a destructive editor edit: the waveform updates, the tab goes dirty, and Ctrl+Z in the editor undoes it - exactly like using the Gain tool. Files without an open tab keep the fast pending-gain path (essential for very large lists), unchanged.
- Opening an editor tab for a file that has a pending list gain now bakes that gain into the tab's buffer as a regular editor edit (with undo) the moment decoding finishes, so the editor's waveform finally shows what you will hear and export. The pending value is cleared at that point - playback, save, and export apply the gain exactly once, through the edited samples.

### Graphical EQ / Compressor / Noise Gate
- The EQ, Compressor, and Noise Gate tools (Editor Inspector) and their Effect Graph nodes now lead with interactive plots instead of only numeric fields (the DragValues/sliders stay for exact entry):
  - EQ: log-frequency response curve (20 Hz - 20 kHz, +/-24 dB) computed from the actual RBJ biquad chain, with three draggable band handles (orange low shelf, green mid, purple high shelf) - horizontal drag sets frequency, vertical sets gain, scrolling over the mid handle adjusts Q.
  - Compressor: static transfer curve (input dB -> output dB with a unity reference diagonal); drag the orange knee horizontally to set the threshold and the green top endpoint vertically to set the ratio.
  - Noise Gate: gate transfer curve with the closed region shaded; drag the handle to move the threshold.

### Effect Graph: Band Split / Band Join and MS Split / MS Join
- Band Split (Routing) splits audio into low / mid / high bands at two adjustable crossovers (log sliders, defaults 200 Hz / 2 kHz). The split is complementary around zero-phase Butterworth low-passes (filtfilt), so the three bands sum back to the input bit-for-bit: Band Split wired straight into Band Join returns the original audio. Each band keeps the input's full channel layout, so per-band processing (e.g. compress only the lows) preserves stereo.
- Band Join sums whatever bands are connected back into one bus (unconnected bands are simply absent).
- MS Split encodes stereo into mid (L+R)/2 and side (L-R)/2 buses; mono passes through as mid with a silent side, and inputs wider than stereo use the first two channels (with a runtime warning). MS Join decodes mid + side back to L/R - straight from MS Split it reconstructs the original stereo exactly, and with only mid connected it produces a mono-in-stereo signal, enabling classic MS tricks (widen/narrow, mid-only EQ) as graph routing.

### Spectrogram: Image-Like Spectral Warp (Spec / Log views)
- New "Spectral Warp" section in the Inspector for the linear and log spectrogram views (the views that resynthesize back to a waveform; Mel stays view-only). Enable "Edit warp points on spectrogram" and drag directly on the spectrogram to push frequency content up or down, liquify-style: each stroke becomes an arrow (origin ring -> target dot) with Gaussian falloff in time and frequency, controlled by the Radius (ms / Hz) fields. Grab an arrow to re-adjust it; double-click or right-click removes it.
- Processing runs in the STFT domain (2048/75% Hann WOLA, same engine as the RX-style spectral mute): a backward frequency remap per analysis frame with complex-bin interpolation and per-bin cumulative phase rotation (phase-vocoder style) so shifted partials stay coherent; only the influenced time region is processed and its edges crossfade against the original. Releasing a drag renders the warp on a worker thread and auditions it immediately (green waveform overlay with "Waveform overlay" enabled); Apply bakes it destructively with full undo and re-analyzes the spectrogram.

### Editor Inspector: Gain Curve, Speed Tool, and Selection-Aware Pitch/Stretch/Reverse
- The Gain tool can now apply a DAW-automation-style gain curve instead of only a uniform value: enable "Gain curve (draw on waveform)" and click the orange polyline on the waveform to add breakpoints, drag them to shape the curve (piecewise-linear in dB, +/-24 dB), double-click or right-click a point to remove it. The curve previews live (green overlay + audition) and Apply bakes it destructively with full undo. Long clips preview the curve by scaling the overview bins.
- New Speed tool (Inspector, between Time Stretch and LoudNorm): tape-style playback-rate change (0.25x-4x) that shifts pitch and length together, using the existing offline resampler. Same preview/apply flow as Time Stretch, including background preview for long clips and session persistence of the rate.
- PitchShift, TimeStretch, and Speed now apply to the current selection when one exists (whole file otherwise). The selection is processed on its own and spliced back with short equal-power crossfades at both joins, so the audio connects cleanly even when the segment shrinks or grows; preview renders the exact same splice you get on Apply.
- Canvas gestures for the preview workflow: with PitchShift active, drag the horizontal pitch line up/down over the waveform (up = higher, +/-12 st, live semitone readout) and release to render the preview. With Speed/TimeStretch active and a selection, grab the selection's right edge and drag left/right to shrink/stretch it - a ghost region and "x1.25 (slower/longer)" readout track the drag, and releasing the mouse renders the stretched waveform and audition.
- Reverse is selection-aware: with a range selected, Preview/Apply reverse only that range, blending a few milliseconds at each join so the reversed span connects without clicks; without a selection it reverses the whole file as before.

## 0.20260708.0 - 2026-07-08

### Forge-Style Processing Chain: Noise Gate / EQ / Compressor / Trim / Bit Depth / Resampler
- Six new nodes in the Effect Graph - Noise Gate, EQ, Compressor, Trim, Bit Depth, and Resampler - plus matching Noise Gate/EQ/Compressor tools in the Editor's Inspector panel, so the same "Forge-style" mastering chain (gate -> EQ -> compress -> trim silence) can be built either as a reusable node graph or applied directly to a tab. Noise Gate and Compressor are envelope-follower designs (threshold/attack/release, plus ratio/makeup for the compressor); EQ is a fixed low-shelf/mid-bell/high-shelf topology (RBJ biquads in series) rather than a freeform band count. Trim reuses the existing Auto Trim detector to remove leading/trailing silence only (internal quiet gaps are left alone). Bit Depth previews 16/24-bit quantization in-buffer (true kbps bitrate remains an export-only concept, since it only applies to a lossy codec, not floating-point audio in the graph); Resampler exposes target rate and Fast/Good/Best quality via the existing rubato-based resampler. All three level/dynamics tools share one DSP implementation (`wave.rs`) between the graph node and the Inspector tool, with live preview/audition and full undo on Apply.

### PluginFX Reliability and a Shared Probe-Status UI
- Native VST3/CLAP parameter probing now retries up to 3 times before falling back to the zero-parameter generic backend, fixing the most common cause of "the plugin's parameters show up sometimes and not other times" - native probing launches the plugin in a separate process and is inherently racy (module load / COM init / plugin init timing), and a single transient failure used to permanently downgrade that probe to Generic.
- Added a "Load from file..." picker to both the Effect Graph's Plugin FX node and the Editor's Plugin FX tool, so an empty (never-scanned) plugin catalog is no longer a dead end - picking a `.vst3`/`.clap` directly adds it to the catalog and probes it immediately.
- The two Plugin FX UIs (graph node and Editor tool) now share one `ui_plugin_probe_status` widget for the error / generic-fallback-warning / backend-log display, so a probe failure reads identically in both places.

### Clipboard/Export Consistency, Clear Edit, and Loop Edit / Inspector Polish
- Fixed clipboard copy (Ctrl+C) silently using a file's original bytes when it only had a pending list-level gain change (no open Editor tab) - drag-export already applied that gain correctly, and copy now shares the same `apply_gain_and_resample` logic instead of a narrower path, so Copy, drag-out, and Export always agree on what "the current version of this file" means.
- Added a "Clear Edit" button next to Undo/Redo in the Editor: reverts a tab's audio to the original file on disk and wipes its undo/redo history in one step (selection, markers, and loop points are left untouched).
- Recent Sessions now remembers the last 10 sessions instead of 3.
- Loop Edit panel: Auto Detect's candidate list is capped to the top 3 (already score-ranked) results instead of every candidate found, and the whole Auto Detect section moved below Seam Check, since it was the least reliable, most-scrolled part of the panel. The "Loop Range" status rows no longer render as their own sub-section header.
- The Inspector panel no longer reserves a tall empty box under short tool content (e.g. Loop Edit with only a couple of Auto Detect candidates) - it now sizes to its actual content.
- The ambiguous single "Edge fade" control in the spectral selection tools (which silently mixed a time-domain and a frequency-domain parameter under one label) is now two clearly labeled "Time fade" / "Freq fade" rows under a "Spectral Mute Fade" heading.
- Investigated (root cause identified, not yet fixed) an occasional waveform/spectrogram visual misalignment at high zoom: the spectral viewport renderer snaps its sample-range bounds down to the nearest analysis-frame boundary via integer division, while the waveform lane renders the exact requested sample range - a real but separate bug from this release's fixes.

### Hitch-Free Loading (no stalls during or right after big loads)
- Loading a 1M-file folder no longer produces multi-hundred-ms frame stalls mid-scan. The path->id index is now keyed by a precomputed 64-bit hash (`types::PathIndex`): growing a plain `HashMap<PathBuf, _>` re-hashes every key, which cost ~270ms in one frame at 640k entries; growing the u64-keyed table only moves slots (the worst load-time frame at 1M drops from ~650ms to ~64ms). Hash collisions degrade a slot to a tiny vector, never to a wrong answer. The remaining per-item maps (id index, folder intern, inflight set, stat cache, SR probe cache) switch to FxHash.
- The list containers pre-reserve toward the scanner's live discovery count (shared via an atomic, not the message channel, so it runs ahead of the budgeted appends).
- Loading a new folder over an existing large list no longer freezes while ~1GB of old items drop: the old containers are handed to a low-priority thread.
- Finishing a scan with no active search no longer re-collects the whole id list (files/original_files are already maintained incrementally during the scan).
- The async sort's snapshot (1M keys + names) is now returned to the UI thread and freed a slice per frame; freeing it wholesale on the sort worker contended with the UI thread inside the allocator and showed up as a ~200-260ms frame right when a background sort finished (now worst ~15ms).
- `NEOWAVES_BENCH_TRACE=1` enables coarse per-stage frame tracing (scan ingest, list jobs, reserve, workspace pass) used to find these; it is compiled in but env-gated.

### 1M-File Responsiveness Pass (priority scheduling, async sort/filter, windowed list)
- Background workers no longer compete with the UI thread for CPU. `lower_current_thread_priority` now works on Linux (per-thread nice) and macOS (utility QoS) in addition to Windows, and is applied to the workers that previously ran at normal priority: the metadata decode pool (also capped at cores-1), list-preview prefetch, LUFS recalc, exports, auto-trim, loop detect, and the folder scan walker. This was the root cause of "buttons stop responding while background work runs".
- Sorting and search filtering never block the UI thread on large lists anymore: the sort snapshot is built in 2 ms slices per frame and the O(n log n) sort runs on a worker thread (results are dropped if the list changed meanwhile); the search filter runs as a sliced per-frame job. Lists <= 50k rows keep the synchronous path.
- The metadata pool queue was rebuilt as a per-path task map with high/low priority lanes: enqueue / promote / dedupe / cancel are all O(1) (promoting a visible row used to scan the whole queue under the mutex every frame), and tasks are now cancellable (list removals and renames cancel their pending decodes; running tasks stop at the header/decode stage boundary).
- Repaint policy: progress-only states (scanning, exports, CSV, AI analysis, sort/filter jobs) repaint at 50 ms instead of forcing 60 fps; metadata streaming repaints at 15 fps; the per-frame metadata drain is time-capped (~1 ms).
- The list is now rendered as a row-index window with an app-managed scrollbar instead of one giant egui scroll area. egui stores scroll offsets as f32, which quantizes above ~16.7M px of content - at 1M rows (48 px cover-art rows = 48M px) scrolling and scroll-to-row broke down. The window start row is a usize, the custom scrollbar maps in f64, and only the visible rows are ever handed to the table, so precision is exact at any list size. Wheel scrolling snaps to whole rows.
- MediaItem slimmed for 1M-file lists (~40% smaller resident footprint): FileMeta is boxed (rows without metadata no longer pay ~200 inline bytes), the three per-item lowercased search-cache strings are gone (the filter lowercases on the fly inside its budgeted slices), external CSV/Excel values are Option<Box<...>>, and folder display names are interned per directory (Arc<str>).
- select_and_load uses the TTL-cached file-exists check instead of a blocking stat() per click/keypress.

### New Loudness Metrics: dBTP / LUFS-S / LUFS-M (+ BS.1770 audit)
- Three new default-hidden list columns - "dBTP" (true peak), "LUFS-S" (max short-term, 3 s), "LUFS-M" (max momentary, 400 ms) - with sorting, CSV export, session persistence and column-picker support. Values are computed in the same full-decode metadata pass as LUFS (I) and shift with pending gain like the existing LUFS/peak columns.
- True peak follows BS.1770-4 Annex 2: polyphase windowed-sinc oversampling (4x below 96 kHz, 2x below 192 kHz) on the original-rate channels; momentary/short-term follow EBU Tech 3341 (ungated maxima).
- Audited the existing LUFS implementation against BS.1770-4: the 48 kHz K-weighting coefficients, 400 ms / 75% overlap blocks, and the -70 LUFS absolute + -10 LU relative gates are spec-correct. Two deviations found: (1) surround channel weighting was missing - now fixed for assumed 5.1/7.1 film layouts (LFE excluded, surrounds x1.41 power weight); (2) the internal 48 kHz conversion uses linear interpolation (~0.1 LU worst case vs a sinc resampler) - kept for speed and documented in code.
- New unit tests: EBU Tech 3341 reference tones (-23 / -33 LUFS at 48k and 44.1k), gating vs silence, burst momentary > short-term, inter-sample true-peak recovery (fs/4 sine at 45 deg phase reads ~0 dBTP from -3.01 dBFS samples), and 5.1 surround weighting (+1.49 dB, LFE gated out).

### 500k-File List Scalability Pass
- Fixed the app effectively locking up after loading very large libraries (reported with 500k FLAC files) once a sort column was involved:
  - Clicking a metadata sort header (Length / SR / Bits / LUFS...) used to enqueue one decode job per row up front - at 500k rows that meant half a million full-file decodes queued in one frame, days of background CPU, and a UI-thread promote scan over the giant queue every frame. Sort prefetch now streams through the existing per-frame pump under its queue budget and inflight cap.
  - Sort keys that are fully answerable from the file header (duration, channels, sample rate, bits, bitrate, BPM tag, created/modified) no longer trigger full-file decodes at all during sort prefetch; only dBFS (Peak) and LUFS sorts still decode, since they need sample data. Files whose header cannot resolve a duration still fall back to one decode pass under a Length sort.
  - The one-shot list sort is ~3x faster at 500k rows (unstable sort with no merge buffer; the equal-key tie-break is now display name then MediaId instead of display name then component-wise `Path::cmp` - numeric sorts like SR tie constantly, so the tie-break dominated the whole sort). A 500k-row File-header click drops ~1.7 s -> ~0.55 s in release. Rows with identical primary key and name now order by scan order rather than full path; the order stays deterministic.
  - Fixed a hidden double sort: the first metadata batch arriving right after a header click passed the "never sorted yet" debounce check and re-sorted the entire list again in the same frame (another ~1.3 s at 500k). Any explicit sort now stamps the debounce clock. Re-sorts while metadata streams in also scale their debounce with the measured cost of the previous sort (8x, capped at 8 s), so the UI thread can never spend the majority of its time re-sorting.
- Bounded the visible-row metadata decode backlog on large lists (>= 8k files): fast scrolling used to enqueue an unbounded pile of full decodes (one per row that ever became visible). New tasks are rejected past a cap and visible rows self-heal by re-requesting, so the queue - and the per-frame promote scan over it - stays small.
- The idle sort-prefetch walk is capped per frame (8192 rows, wrapping cursor) so a fully-resolved 500k list no longer pays an O(n) scan every frame while a metadata sort is active.
- Select-all + Enter on a huge list no longer funnels every selected path through the tab-open path (and its per-path skip log) once the editor tab limit is reached.
- CSV export now streams its metadata jobs to the worker pool frame by frame instead of mass-enqueueing every row up front (new regression test). This keeps huge exports compatible with the backlog cap - and fixes a pre-existing stall where a large-list export with a dBFS/LUFS background mode active could drop most of its decode jobs at the old cap and never finish.
- Added an opt-in headless benchmark (`tests/large_list_bench.rs`) that loads a 500k-file fixture and reports scan/append frame times, steady-state frame cost, sort latency, and RSS.

## 0.20260706.0 - 2026-07-06

### Fix: List Randomly Turning Red (dev builds)
- Fixed the file list sometimes getting 2px red outlines around every cell in debug builds. egui keeps separate dark/light styles and follows the OS theme by default; the startup style patch (app text sizes + disabling the `warn_if_rect_changes_id` debug heuristic that false-positives on the virtualized list) only landed in the style slot active at startup. When Windows later reported the other theme, egui swapped in the unpatched style - the app still looked dark (visuals were re-applied every frame) but the debug heuristic came back on and painted red outlines after scroll jumps. Styles are now patched via `all_styles_mut` (both slots), theme visuals likewise, and a kittest regression simulates the OS theme flip and asserts no red debug rects are painted.

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
