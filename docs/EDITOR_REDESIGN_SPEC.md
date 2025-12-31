# Editor Redesign Spec (v3)

Purpose
- Define a clear, testable editor UX and data model.
- Reduce heavy rendering and unstable preview behavior.
- Make loop and fade controls visually obvious as overlays on the waveform.

Observed Issues (current)
- Editor is heavy and can stutter on long files.
- Loop and fade are not visually obvious or are hard to understand.
- Playback can break when preview or edit state changes while playing.
- Editing state is loosely defined; multiple edits can overlap without clear rules.

Goals
- Smooth interaction at 60 fps for typical clips.
- Clear visual overlays for loop, trim, and fades.
- Predictable preview and playback state transitions.
- One active editing session at a time, with explicit tab state.

Non-Goals (for this phase)
- Multi-track mixing.
- Real-time spectral editing tools beyond display.
- High quality SRC beyond current engine.

Core Concepts
- Editor Tab: data and UI for one file.
- View: Waveform | Spectrogram | Mel (view only in this phase).
- Tool: active editing tool within the view.
- Session State: Idle, Playing, Previewing, Processing.

Layout
- Top row (inside editor):
  - File name + dirty marker
  - Transport: Play/Pause, time readout
  - Loop mode: Off / Marker / Whole
  - View mode: Wave | Spec | Mel
- Main area:
  - Timeline and channel lanes
  - Overlays for loop, trim, fade
  - Playhead
- Inspector (right):
  - Tool selector
  - Tool parameters
  - Apply / Preview / Cancel

Time Mapping
- Samples are the source of truth for time.
- Visible window: view_offset .. view_offset + visible_samples.
- Mapping:
  - sample = view_offset + (x / wave_w) * visible_samples
  - x = wave_left + (sample - view_offset) / samples_per_px

Waveform Rendering
- Use LOD:
  - spp >= 1.0: min/max columns
  - spp < 1.0: polyline
- Overlays are drawn after base waveform.
- All overlays share the same time mapping.

Overlay Spec (Waveform)
1) Loop Region
   - Two vertical markers (S/E).
   - Shaded region between markers (low alpha).
   - Crossfade bands at start/end with distinct colors.
   - Label text near markers with duration.
2) Trim Region
   - Kept region shaded lightly.
   - Outside region dimmed and desaturated.
   - Handles on A/B points for drag.
3) Fade In / Fade Out
   - Gradient overlay in the fade region.
   - Fade curve line drawn on top (linear or equal power).
   - Duration label in ms or seconds.

Playback and Preview State Machine
- Idle: no playback, no preview.
- Playing: normal playback of base buffer.
- Previewing:
  - Preview buffer replaces base buffer.
  - When preview ends or tool changes, restore base buffer.
- Processing:
  - UI input is blocked (busy overlay).
  - When done, switch to Idle and update base buffer.

Rules
- Only one preview at a time.
- Starting playback cancels preview unless tool explicitly supports preview playback.
- Editing while playing:
  - Tool changes stop playback first.
  - Preview requests while playing stop playback and enter Previewing.

Tool Behavior (Waveform)
- Loop Edit:
  - A/B markers adjustable by drag and by playhead set.
  - Crossfade preview overlay shown on waveform.
- Trim:
  - A/B region shown as "kept" overlay.
  - Preview: only the kept region plays.
  - Apply: destructively keeps A/B region.
- Fade:
  - Separate Fade In and Fade Out.
  - Preview: overlay + audio preview if clip is small enough.
  - Apply: destructively applies.
- Gain / Normalize:
  - Preview: overlay + audio preview if clip is small enough.
  - Apply: destructively applies.
- Reverse:
  - Preview: overlay + audio preview if clip is small enough.
  - Apply: destructively reverses.

Spectrogram View
- Display only (no edits in this phase).
- Compute spectrogram asynchronously.
- Reuse current view_offset and zoom for time alignment.
- Mel view uses log-mel mapping over the same spectrogram data.
- If clip is large, show a message and do not compute.

Tab and Session Policy
- Only one tab can be in "editing" mode at a time.
- Other tabs are read-only (view and playback allowed).
- Switching from a dirty tab prompts:
  - Leave (discard preview only, not saved edits)
  - Cancel
- Apply actions always mark tab dirty.

Performance Targets
- Spectrogram compute is capped (max frames).
- Preview is disabled above a safe sample threshold.
- UI redraw is capped to 60 fps, with input priority.

Instrumentation
- Debug overlay that can show:
  - view_offset, spp, visible window
  - preview state, processing state
  - spectrogram cache status

Acceptance Criteria
- Loop and fade overlays are visible and understandable without opening inspector.
- Playback does not glitch when changing tools.
- Switching tabs preserves state and avoids multiple active edits.
- Spec/Mel view renders within 1 second for typical clips.
