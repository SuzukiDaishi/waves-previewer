# Editor 2.0 — Multichannel, dB Grid, Seek and Time‑Zoom

This document captures the agreed UX and the technical outline for the upcoming editor update. It is scoped to UI and visualization; the audio callback remains unchanged and real‑time‑safe.

## UX Summary

- Multichannel lanes
  - Render one lane per channel, stacked vertically.
  - A single shared playhead (seek bar) across all lanes.
- dB reference grid
  - 2–3 subtle horizontal lines per lane (e.g., 0, −6, −12 dBFS) with small labels in a left gutter.
- Mouse seek/scrub
  - Click to seek; click‑and‑drag to scrub horizontally. Playback state (playing/paused) is preserved.
- Time zoom and pan
  - Ctrl + Mouse Wheel: zoom in/out centered at cursor.
  - Shift + Mouse Wheel: horizontal pan (scroll).
  - Right‑drag or Middle‑drag: horizontal pan (fallback).
  - Double‑click: toggle fit whole / restore last zoom.

## Interaction Details

- Seek mapping
  - Local x → sample index: `seek = view_offset + x * samples_per_px` (clamped to file length).
  - Apply seek to `AudioEngine.play_pos_f` atomically.
- Zoom mapping (Ctrl+Wheel)
  - `samples_per_px *= zoom_factor` (`zoom_factor` ≈ 0.9 or 1.1 per wheel notch).
  - Recenter so the cursor time stays fixed: recompute `view_offset` from the cursor time before/after.
- Pan mapping
  - `view_offset += delta_px * samples_per_px` (clamped to `[0, samples_len)`), from Shift+Wheel or right/middle drag.

## Data Model Additions (per editor tab)

- `waveform_minmax_ch: Vec<Vec<(f32,f32)>>` — per‑channel min/max bins used for drawing; may be rebuilt for the visible window.
- `samples_len: usize` — total length in samples.
- `view_offset: usize` — first visible sample index.
- `samples_per_px: f32` — time zoom (samples per pixel). Visible samples ≈ `width_px * samples_per_px`.
- `drag_seek: Option<f32>` — transient normalized x while scrubbing.

## Rendering Outline

- Split viewport into a fixed left gutter for dB labels and a right area for channel lanes.
- For each lane:
  - Build bins for the visible subrange only: `bins = width_px` over `[view_offset .. view_offset + visible]`.
  - Convert dB values to amplitude by `amp = 10^(dB/20)` to place horizontal grid lines.
  - Draw min/max vertical strokes per x column with amplitude‑based colors (existing palette).
- Playhead
  - `x = (play_pos - view_offset) / samples_per_px`; draw as a 2px line across all lanes.

## Decode/Preparation

- Add `decode_wav_multi(path) -> Result<(Vec<Vec<f32>>, u32)>` for visualization. Playback can remain mono for now.
- Initial version may compute visible bins on the fly; later we can add a small cache keyed by `(channel, zoom_rounded, segment)`.

## Performance Notes

- Start with per‑frame on‑the‑fly binning (bins ≈ panel width) which is fast in practice.
- If profiling shows hotspots for very long files or many channels, add:
  - Simple tile cache for the visible window.
  - Optional background multi‑scale (mip) min/max generation.

## Out of Scope (for this update)

- Per‑channel solo/mute, gain envelopes, spectral views, AB loop UI, and edit operations remain future work.

## Editing Roadmap (Planned)

This section outlines planned, non‑destructive editing features to add to the editor. Scope and details are draft and subject to change.

- Waveform editing
  - Trim (in/out) with front/back fades
  - Front/back crossfade at cut points
  - Loop markers (A/B) with seamless loop playback; optional crossfade at loop boundary

- Spectrogram (editing)
  - Region/brush selection for noise reduction (attenuation within selected band/time)
  - Frequency‑axis image‑like warp (horizontal warp in spectrogram domain)

- Mel‑Spectrogram
  - View‑only (no direct editing in first phase)

- WORLD features (speech)
  - F0 sample‑level editing (pitch curve)
  - Spectral envelope: frequency‑axis image‑like warp

### Planned UI Layout

- Below the top toolbar, add an “Edit” tab bar for per‑file editing panels.
- Under the edit tabs, show editing controls (e.g., Trim, Loop markers) relevant to the active panel.
- Below controls, stack visual panes vertically: Waveform, Spectrogram, Mel‑Spectrogram, WORLD (F0/envelope).
  - All panes share the same time axis and playhead; synchronized zoom/seek.

Notes
- Editing is planned to be non‑destructive; operations apply to a working buffer and can be previewed/rolled back.
- Heavy operations (noise reduction, warps) will run on a background worker and update the preview when complete.
- Further details will be specified in a dedicated editing document.
