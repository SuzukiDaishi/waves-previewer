# Implementation Plan (v1)

This plan turns the redesign specs into incremental, testable work.

Phase 0 - Baseline and Instrumentation
- Add small perf counters (fps, frame time, worker queue size).
- Add a debug overlay toggle for list and editor.
- Create reproducible test commands (existing WAV and dummy list).

Phase 1 - List Interaction and Playback
- Fix click selection for all cells.
- Add keyboard selection auto-scroll with margin.
- Add page up/down and home/end.
- Introduce fast note preview for long files:
  - Decode a small chunk and start playback.
  - Continue decoding in background.
  - Force Speed mode for list preview.
- Add acceptance tests for list behavior.

Phase 2 - Editor UI Overlays (Waveform)
- Loop overlay: markers, region shade, crossfade bands, labels.
- Trim overlay: kept region shade, dim outside, handles.
- Fade overlay: gradient and curve line, duration labels.
- Add handle hit testing and drag updates.

Phase 3 - Editor State Machine
- Formalize state transitions: Idle, Playing, Previewing, Processing.
- Cancel preview on tool change or playback start.
- Prevent multiple preview jobs in parallel.
- Add explicit edit session ownership for tabs.

Phase 4 - Tab and Session Policy
- Enforce single editable tab at a time.
- Add prompts on dirty leave/close.
- Make background tabs read-only.

Phase 5 - Spectrogram and Mel View
- Cache and render spectrogram tiles.
- Add time alignment to view_offset.
- Add Mel mapping view-only.
- Provide disabled message for large clips.

Phase 6 - Performance and Cleanup
- Profile list and editor drawing.
- Optimize hot paths (binning, overlay draw, text).
- Add quick meta fallback to avoid inflight stalls.

Testing Plan
- List:
  - Click select and keyboard select are consistent.
  - Auto-scroll works on large lists (dummy list 300k).
  - Playback starts quickly on long files.
- Editor:
  - Loop overlay matches inspector values.
  - Fade/Trim overlays align to waveform.
  - Playback does not glitch when switching tools.
- Tabs:
  - Dirty prompt appears correctly.
  - Only one active editor tool set.

Acceptance Criteria
- List selection always visible and reliable.
- Editor overlays are clear and intuitive.
- Playback stays stable under tool changes.
