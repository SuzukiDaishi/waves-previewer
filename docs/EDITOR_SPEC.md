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
  - For time rendering, switch by zoom level (`spp = samples_per_px`):
    - `spp >= 1.0` (overview/normal): build `bins = width_px` over `[view_offset .. view_offset+visible]` and draw min/max vertical strokes per pixel column.
    - `spp < 1.0` (fine zoom): draw per‑sample polyline connecting adjacent samples; when `pixels_per_sample >= 6`, also draw stems from the centerline to each sample for clarity.
  - Convert dB values to amplitude by `amp = 10^(dB/20)` to place horizontal grid lines.
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

- Per-channel solo/mute, gain envelopes, spectral views, AB loop UI, and edit operations remain future work.

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

---

# Hierarchical Editing UI (Views → Tools)

This section formalizes the editing UX requested after the initial spec. The editor
is organized as a hierarchy: first choose the view (Waveform/Spectrogram/Mel/WORLD),
then choose a tool specific to that view. All views share the same time axis,
playhead and A/B loop markers. Switching the view automatically swaps the active
toolbar and inspector while keeping time/loop context.

Levels
- Editor Tab (per file): playback, loop, zoom/pan, shared state lives here.
- View (second level): Waveform | Spectrogram | Mel | WORLD (planned).
- Tool (third level, view dependent): editing action that exposes its own overlay
  and parameters in the Inspector.

View → Tools (MVP and planned)
- Waveform: Seek/Select, Loop Edit (A/B), Trim, Fade (in/out, curve), Gain,
  Normalize (dBFS; LUFS later), Reverse, Silence (insert/mute), Pitch Shift,
  Time Stretch.
- Spectrogram: Seek/Select‑2D (rect first), Noise Reduction (attenuation within
  selection), Spectral Attenuation, Repair/inpaint, Frequency‑axis warp.
- Mel: View‑only in MVP; pitch contour editing later.
- WORLD: F0 edit, spectral envelope warp (planned).

Shared interactions
- Space: Play/Pause. A/B: set markers. L: Loop toggle. Ctrl+Wheel: Zoom.
- Shift+Wheel / Middle/Right drag: Pan. Double‑click: fit/restore.
- Tools may require a selection; if none exists they use A–B, otherwise Whole.

Shortcuts (proposal)
- Views: 1=Waveform, 2=Spectrogram, 3=Mel, 4=WORLD
- Tools: Q=Seek/Select, W=Loop Edit, E=Trim, R=Fade, T=Gain,
  Y=Noise Reduce (Spec), Esc=Cancel tool

State/Mapping Rules
- Changing the View resets the active tool to Seek/Select for that view.
- Time selection carries across views; Spectrogram may keep a 2D selection whose
  time component is preserved when switching away.
- A/B loop has priority when Loop is enabled. Zero‑cross snapping can be toggled
  and applies to time edges (Waveform); spectral tools ignore it.

Inspector
- The right‑side inspector shows the active tool’s parameters and target range
  (Selection / A–B / Whole) with Apply/Preview/Cancel controls.

Background jobs
- Heavy tools (Pitch/Stretch/NoiseReduce, etc.) execute on a worker thread.
  The busy overlay blocks input and shows progress; parameter drags are debounced
  (≈200 ms) and only the latest job runs.
