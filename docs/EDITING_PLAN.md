# Editing UX/Implementation Plan (MVP → Extend)

This document captures the concrete plan for waveform editing in waves-previewer.
It follows a hierarchical UX (View → Tool). First pick a View (Waveform /
Spectrogram / Mel / WORLD), then pick a Tool for that view. All views share
time/playhead/A–B markers. See docs/EDITOR_SPEC.md for canvas/interaction details.

- Goals: instant preview, non-destructive ops, seamless loop, background heavy jobs.
- References: Sound Forge (time selection, zero-cross, AB loop), iZotope RX (spectral
  view and region operations), Wwise (loop markers and non-destructive workflow).

Hierarchy overview
- View selector (Wave/Spec/Mel/WORLD) right under the editor toolbar.
- Tool selector (contextual) appears next to it; contents change with the view.
- Inspector (right) shows the active tool’s parameters and range.

MVP scope
- Time selection on the editor canvas (drag), shift-extend, double-click = select all.
- AB loop markers (A/B keys). Loop toggle (L). Optional zero-cross snap (S).
- Inspector (side panel) with actions: Trim, Gain, Normalize(dBFS), Fade In/Out,
  Reverse, Silence (insert/mute). Apply to Selection / Whole / A–B.
- Export Selection. Heavy operations (Pitch/Stretch) stay in background worker.

Data model additions (per EditorTab)
- selection: Option<(usize, usize)>
- ab_loop: Option<(usize, usize)>
- view_mode: Waveform | Spectrogram | Mel
- snap_zero_cross: bool
- drag_select_anchor: Option<usize>

Interactions
- Click to seek; Left-drag to select; Ctrl+Wheel to zoom; Shift+Wheel or Middle/Right
  drag to pan; Double-click to fit or select all; A/B to drop loop markers; L to toggle loop.

Rendering
- Overlay the selection band and AB markers on top of multi-channel lanes.
- Spectrogram view starts as visualization-only; zoom/pan/seek shared with waveform.

Phases
1) MVP above + Export Selection
2) Spectrogram visualization + rectangular selection (time first; frequency later)
3) Pitch/Stretch apply to selection (flush tail; replace preview buffer)
4) SR/BitDepth conversion + TPDF dither; streaming export for long files
5) Undo/Redo; regions; spectral brush/lasso; noise reduction
