# Editor Inspector

The right-hand pane of the editor: choose a tool, see what it will act on,
set it up, hear it, apply it. This document is the contract the code
follows; read it before changing a tool, the preview, or undo.

## Code map

| Concern | Where |
|---|---|
| What each tool is (name, icon, category, target rule, preview kind, flags) | `src/app/editor_tool.rs` (`ToolKind::spec`, `ToolCategory`) |
| Samples in, samples out, per tool -- shared by preview and Apply | `src/app/tool_render.rs` (`ToolOp`, `render`, `TimeMap`) |
| Building an op from a tab, the target and channel mask, committing a render | `src/app/tool_apply_ops.rs` |
| The tool being adjusted: draft, draft history, preview state | `src/app/tool_session.rs` (`ToolSession`, `DraftSnapshot`, `PreviewKey`) |
| The preview engine: debounce, render on a worker, install under the playhead | `src/app/tool_preview_ops.rs` |
| What the Inspector asks for, carried out (select tool, Apply, Discard, tool buttons) | `src/app/inspector_ops.rs` (`InspectorAction`, `ToolCommand`) |
| The UI | `src/app/ui/inspector/` (`mod.rs` shell and footer, `widgets.rs`, `tools/*.rs` panels, `views.rs` analysis views) |
| Automation of the EQ / Compressor / Limiter / Clipper: which settings follow a curve, the lanes on a tab | `src/app/tool_automation.rs` (`AutoParam`, `AutomationLane`); the DSP side is `src/automation.rs` (`Curve`, `Tracks`, `Evaluator`) |
| Oversampling of the Brickwall Limiter and the Clipper | `src/oversample.rs` (`Oversampling`, `Oversampler`, `UpStream`, `DownStream`); the tools run on it in `src/dynamics.rs` |
| The Reverb: settings and DSP; IRs; the library; in the app; the panel | `src/reverb.rs`; `src/impulse.rs` (`IrRef`, `IrId`, `ImpulseResponse`); `src/app/ir_library.rs`; `src/app/reverb_ops.rs`; `src/app/ui/inspector/tools/reverb.rs` |
| Breakpoint curves on the waveform (Gain, Pitch Shift, automation) | `src/app/ui/curve_canvas.rs` (`edit_breakpoint_curve`, `draw_breakpoint_curve`) |

`ui_editor_view` (`ui/editor.rs`) only calls `ui_editor_inspector` where the
pane goes, after handing it what the canvas changed this frame
(`note_canvas_draft_change`).

## Layout

Top to bottom:

1. **Palette.** Category chips (Edit, Level, Time/Pitch, EQ/Dynamics, Repair,
   Channels, Loop/Markers, Analyze/FX; in Spec/Freq Log views: Spectral,
   Repair, Loop/Markers), then the named tools of the category shown. A
   category opens on the tool last used in it.
2. **Header.** Icon and name, a one-line summary, `⟲` (reset this tool's
   settings, one draft step) and `?` (what the canvas does with this tool).
3. **Target bar.** What the tool acts on: `Selection 0:01.200–0:03.400
   (2.200 s)`, `Whole file`, `Insert at …`; the channels (`All channels`, the
   Custom channel view's, or "the length changes"); the frequency band when
   one is selected. `▶` plays only the target.
4. **Settings.** The tool's panel. Panels draw settings only.
5. **Footer** (never scrolls). What is heard, `Auto`, then Preview / A/B …
   Discard / Apply.

The editor toolbar carries `⟲ ⟳` (tooltips name the step). Edit menu:
Undo, Redo, Revert to Saved…, History…, Regions….

## Tools

Every tool has one `ToolSpec`:

- **Target rule.** `SelectionOrWhole` (the selection, else the whole file),
  `WholeOnly` (the selection is ignored, and the target bar says so),
  `SelectionOnly`, `InsertPoint` (selection start, else the playhead),
  `None` (canvas or annotations).
- **Preview kind.** `Rendered` (the engine below), `Live` (the Panner's
  matrix in the output callback), `Draft` (the Pencil's draft is what is
  heard), `Own` (Music Analyze, Plugin FX: their pipelines, driven by the
  footer), `None` (Trim, Channel Routing, Markers, Note).
- **Flags.** `changes_length` (never honours the channel view),
  `honors_channel_view`, `heavy` (auto preview starts manual on long clips).
- **Icons** must be drawn by the fonts the app installs (the bundled Japanese
  face plus egui's defaults); `every_icon_is_drawn_by_the_app_fonts` checks
  it, because a missing glyph is an empty box, not an error.

Spectral Mute and Heal are Repair tools with the `SelectionOnly` rule: the
time selection, and only its frequency band when one is selected. Their edge
fades (`ToolState::spectral_time_fade_ms` / `spectral_freq_fade_hz`) are tool
settings, saved with the session and shared with Play Selection and spectral
copy / paste.

Spectral Warp points and Spectral Brush stamps are spent by an apply: they are
taken off the tab before the commit records the new starting point, so neither
Esc nor a second Apply brings them back to push or cut the same place twice.

Gain, Normalize, Gate, EQ and Compressor honour the selection like every
other `SelectionOrWhole` tool. A partial range is joined to untouched audio
without a click: gain-type tools ramp at interior edges; filters run with
context either side (`RANGE_CONTEXT_SECS`) and splice back with crossfades.

Settings that change nothing (`ToolOp::is_neutral`: Gain 0 dB, rate 1.0, a
flat EQ, …) cannot be applied and have nothing to preview.

## One render, preview and Apply alike

`tool_render::render(ToolRenderRequest)` is the only way a rendered tool turns
samples into samples. It works at the buffer's own rate (`buffer_sr`), on
every channel, and checks a cancel flag between channels. Its output carries
a `TimeMap` that says how positions moved (a range stretched, silence
inserted, a loop written out).

**Apply commits what was heard.** `editor_apply_op_now` and
`spawn_editor_apply_op` first look for a preview whose `PreviewKey` equals the
key of the op they were asked to run (same tab, same audio revision, same
rate, same op fingerprint, target and channel mask); if there is one, its
samples are committed as they are. Otherwise the same `render` runs again --
on the UI thread for light ops, on the single apply slot for heavy ops or
long clips.

`editor_commit_render` is the one commit: one undo step, the channels, and
everything that points into the audio moved through the `TimeMap` (markers,
regions, loop points, selections, fade ranges, notes, the transcript).

## Preview

Session states (per tab, `ToolSession::phase`):

```
            open tool / Apply / Esc / undo
                       │
                       ▼
                    ┌──────┐   a setting changes, or Preview
                    │ Idle │ ───────────────────────────────┐
                    └──────┘                                ▼
                       ▲                             ┌────────────┐
                       └──────── Esc / Apply ─────── │ Previewing │◀─┐ a setting
                                                     └────────────┘──┘ changes
```

- **Opening a tool changes nothing that is heard.** The first change (or the
  Preview button) starts previewing; from then on every change is followed.
- **Debounce.** The engine renders once the settings rest for
  `TOOL_PREVIEW_DEBOUNCE` (200 ms); during a drag it re-renders at that
  interval. A newer request cancels an older render; a result whose key is no
  longer wanted is dropped. The Preview button renders at once.
- **Long clips.** A render for a clip longer than `LIVE_PREVIEW_SAMPLE_LIMIT`
  comes back once, with its audio and an overview waveform (min/max bins)
  instead of a full-sample overlay -- the detail is invisible zoomed out and
  would cost another clip-sized copy. Until it lands the previous preview (or
  the original) stays on screen and the footer says it is rendering; the old
  "overview first, full later" staging is gone.
- **Manual.** With `Auto` off -- the default for `heavy` tools on clips
  longer than `LIVE_PREVIEW_SAMPLE_LIMIT`, unless they are `progressive` --
  a change shows "Settings changed" and Refresh renders.
- **Progressive.** A `progressive` tool (EQ, Compressor, Brickwall
  Limiter, Clipper, Noise Gate: what it does at a moment depends only on
  the audio near it) on a
  target longer than twice its window is rendered in two passes by the same
  worker: first from 1 s before the playhead to 20 s after it
  (`progressive_window`), installed as an incomplete preview -- the rest of
  the file as it was -- then the whole target, which replaces it. The footer
  says "Hearing it around the playhead -- rendering the rest". Apply only
  ever commits a complete preview (`fresh_preview`); one asked for while
  the window is all there is renders the whole target itself. So the
  Limiter, `heavy` as it is, previews automatically on a file of any
  length and is heard about 0.3 s after a setting rests.
- **Under the playhead.** A finished preview is swapped in where the
  playhead is, mapped through the `TimeMap`, with a short fade back in
  (`PREVIEW_SWAP_FADE_MS`); playback does not stop. Under a pitch/stretch
  playback mode it stops instead (there is no seamless swap into a rendered
  copy). If the tab is not playing, Play installs the preview.
- **A/B** switches between the original and the preview at the same place.
- **Tool switch** drops the preview at the same place; playback goes on.
- Spawning is behind `deferrable!`; installing a finished preview is not.

## EQ and dynamics

The EQ, Compressor, Brickwall Limiter, Clipper and Noise Gate run on `crate::eq` and
`crate::dynamics` (not the Effect Graph's `wave::` versions, which stay as
they were so a saved graph keeps rendering the same).

- **One computation, three pictures.** A render of these tools carries a
  `RenderAnalysis` beside its samples: the EQ's average spectrum of the
  processed target, or the dynamics tools' `DynamicsTrace` (input peak and
  gain change per hop). The Inspector draws it, the waveform draws it
  (`render/dynamics_overlay.rs`), and the live meters read it at the
  playhead -- so what is shown is what Apply writes.
- **Before anything renders** the engine measures the source over the
  target on a worker (`SourceAnalysis`, keyed by tab, audio revision, target
  and channel mask): the EQ's "before" spectrum, the dynamics tools' level
  over time.
- **Live.** While the tab plays, the panel reads a window of what is heard
  around the playhead each frame it draws (spectrum at the mini meter's
  cadence; in / out peaks; gain from the trace). Nothing is computed when it
  is not playing.
- **Ranges.** A selection is processed with its own pre-roll -- a few
  release times for the dynamics tools, eight periods of the lowest corner
  for the EQ -- so the envelope or the filter has settled at the first kept
  sample; the edges are spliced as every range edit is.
- **Mid / side** bands need the stereo pair in scope; on anything else a Mid
  band works on every channel and a Side band does nothing (the panel says so).
- **Sessions** store the settings as `eq_parametric`, `compressor_settings`,
  `limiter_settings`, `clipper_settings` and `gate_settings`; a session
  without them is read from
  the old flat fields (three bands, the hard-knee per-channel compressor, the
  gate that shut to silence), which are still written for older builds.
- **A cut's Q** is its resonance, in its own field (`EqBand::cut_q`, 0.4-10):
  0.707 is the flat Butterworth corner and the default, so a cut saved before
  cuts had a Q (and whose `q` is whatever its bell had) sounds as it did. The
  sharpest section of the cascade is scaled; the slope past the corner stays.
  On the graph a cut's handle sits at the resonance it adds, and dragging it
  up or down sets it; the wheel still steps the slope.
- **The Brickwall Limiter** (`ToolKind::Limiter`; the variant keeps its
  name so sessions and scripts that say `limiter` still mean it) drives up
  to +48 dB into a ceiling down to -30 dB; its GR scales show 48 dB once the
  drive needs it (`LimiterSettings::gr_scale_db`).
- **The Clipper** cuts whatever passes the ceiling on the spot: no gain
  riding, no lookahead, no release. Input 0 to +24 dB, ceiling -30 to 0 dB,
  and a knee of 0 to 12 dB under the ceiling where the curve starts to bend
  (a tanh from the knee towards the ceiling, entered at the slope it meets;
  0 is a hard clip). Its trace is measured from what it wrote: per hop, the
  loudest output against the loudest driven input -- so the time graph's
  output line and the "output peak" readout include whatever the filters
  rounded over the ceiling. Its transfer graph has two handles: the corner
  (up and down the ceiling, sideways the input gain) and the knee.

### Oversampling

Both the Brickwall Limiter and the Clipper offer Off, 2x, 4x, 8x and 16x
(`crate::oversample::Oversampling`; sessions write `off`, `x2` ... `x16`).
The limiter defaults to Off -- it renders exactly what it did before there
was a choice, which a test holds bit for bit against the old code -- and
the clipper to 4x, where clipping without it is mostly aliasing.

- **What runs at the high rate.** Everything: the limiter's detector,
  lookahead window, release and its clamp at the ceiling (the lookahead and
  release keep their times; true peak, when on, estimates between the
  oversampled points); the clipper's curve. Both are one generic code path
  over a frame source (`Frames`): the buffer itself, or `UpStream`, the
  buffer interpolated a group of `factor` frames at a time just ahead of
  where it is read. The result goes back down through `DownStream`.
- **The filters.** One Kaiser-windowed sinc per factor, 64 taps per phase at
  the buffer's rate (about 100 dB of stopband, transition band 0.45-0.55 of
  the rate): flat to 19.8 kHz at 44.1 kHz. Linear phase and centred on the
  sample it makes -- offline that costs nothing -- so nothing moves in time
  (tests check a burst lands within half a sample of where the plain tool
  puts it). The interpolator's phase 0 is the original sample, exactly.
- **The ends.** The high rate runs the filters' half-length (32 samples)
  before the first sample and after the last, so the edges are rebuilt
  from all the interpolator made rather than cut. A range render takes
  that much more context (`oversample_reach`).
- **Back under the ceiling.** The filter back down rounds what was cut and
  can ring past the ceiling (a heavily clipped tone by up to about 1 dB).
  The limiter is a brickwall, so it always clamps once more at the buffer's
  rate; its gain is already smooth, so that clamp only touches a hair. A
  clipper's overshoot is too large to clamp: that is clipping again at the
  buffer's rate, and on a clipped 4.1 kHz tone at 8x it brought the
  aliasing from -98 dB back up to -45 dB. So **Hard ceiling** (the default)
  runs the overshoot through a fast linked limiter instead
  (`catch_overshoot`: 1 ms lookahead, 20 ms release -- slow enough that a
  run of clipped crests is held one steady fraction of a decibel down), which
  leaves every sample that was under the ceiling as it was -- and the
  aliasing at -98 dB. Off, the
  overshoot is left for a limiter after it, and the readout says so.
- **Memory and time.** Both directions stream, so memory is a few thousand
  frames whatever the length. Time grows with the factor (the filters cost
  about 128 multiply-adds per factor per sample and channel); the
  progressive preview keeps a long file audible around the playhead first.
- **Automation** curves stay in buffer samples: a high-rate frame reads the
  settings of the buffer sample it belongs to (`Clock`).

## Reverb

A convolution reverb over impulse responses (Analyze/FX). The IRs are
Conner's Impulse Response Library (MIT): the whole library is the git
submodule `vendor/IR-Library`, and the installer copies the selection in
`assets/ir/bundled.txt` (41 files, about 83 MB) to `ir\` beside NeoWaves.

- **Which IR.** `ReverbSettings::ir` is an `IrId`: a process-wide interned
  `IrRef` (`Library(path inside the library)` or `File(path)`), so the
  settings stay `Copy` and the IR travels with them -- through the draft
  history (picking an IR is one undo step), the preview key and the
  session, which writes it as `lib:<path>` or `file:<path>`. A library IR
  is named by its path inside the library, never by where the library is,
  so a session opens on a machine that keeps it elsewhere. `None` is the
  default IR (`impulse::DEFAULT_IR`, a small church, in the selection).
- **Where the library is** (`ir_library::library_roots`), first match wins:
  the folder the user chose (prefs `ir_library_dir`, the "IR folder..."
  button), `ir\` beside the executable, and in a debug build
  `vendor/IR-Library`. The scan and every load run on workers.
- **Loading.** `reverb_ops::tick_reverb` gives the active tab the IR its
  settings name: decoded and resampled to the tab's rate on a worker, kept
  in an eight-entry cache, and put on the tab (`EditorTab::reverb_ir`, not
  part of the draft). Until it is there, `tool_op_for_tab` has no op, and
  the preview waits.
- **Routing.** A stereo target meets a stereo IR channel by channel and a
  true-stereo (4-channel) IR by all four paths, in the order the library
  writes them: L to L, L to R, R to L, R to R (checked against its
  `Stereo Mixer 1 Channel Balance Left/Right Full`: the direct paths are 0
  and 3, and the path into the silenced output is the quieter cross path).
  A mono or multichannel target takes the IR folded to mono.
- **Only the target is sent**, eased in and out where it meets audio that
  is not. Inside the target, Mix sets dry against wet (the dry eased in and
  out at interior edges, as a splice is) and Output scales both; after it,
  the wet is added to whatever is there and the dry is never touched.
- **Tail** (`ReverbTail`): *Ring on* lets the tail sound over what
  follows, and where it runs past the end of the file the file grows to hold
  it -- up to where it has fallen 90 dB under its peak, not to the end of
  the IR. *Keep length* rings over what follows and fades out where the end
  of the file cuts it. *Stop at the end* fades the wet out over the last
  0.1 s of the target: nothing outside it changes. Growing the file is a
  `TimeMap::Range` at the old end, so nothing before it moves; every channel
  grows, a channel out of scope with silence, which is why the Reverb is the
  one length-changing tool that honours the channel view.
- **The IR as set.** Length cuts the IR (faded over the last tenth of what
  is kept), Reverse turns it round, Pre-delay delays the wet, and Normalize
  (on by default) scales it so white noise comes out as loud as it went in.
  These change the IR itself, so none of them follows a curve.
- **Convolution.** Uniformly partitioned overlap-save over realfft, offline:
  a partition about a sixteenth of the IR (1,024 to 32,768 frames), every
  input block transformed once for every path it feeds. Nothing moves in
  time. A test holds it to direct convolution.
- **Not progressive.** A window render around the playhead cannot hold the
  tails reaching into it from before the window. The Reverb is `heavy`
  instead, so on a long file its preview waits for Refresh.

## Automation

The EQ, Compressor, Brickwall Limiter, Clipper and Reverb can let most of their settings follow a curve
drawn on the waveform, like the Gain and Pitch Shift curves:

| Tool | Settings that can follow a curve | Not (why) |
|---|---|---|
| EQ | each band's Freq, Gain (bell, shelves) and Q (a cut's resonance); Output | shape, slope, mode, bypass (switches) |
| Compressor | Threshold, Ratio, Knee, Attack, Release, Makeup, Mix, Range, Link, Sidechain HPF, RMS window | Lookahead (it is the detector's delay), detection, auto makeup (switches) |
| Brickwall Limiter | Input, Ceiling, Release | Lookahead (the window), true peak, link, oversampling (switches) |
| Clipper | Input, Ceiling, Knee | Oversampling, hard ceiling (switches) |
| Reverb | Mix, Output, Width, Low cut, High cut (on the wet after the convolution) | the IR, Pre-delay, Length, Reverse, Normalize (they change the IR itself), Tail |

- **Lanes.** `EditorTab::automation` holds one `AutomationLane` per setting:
  its points `(sample, value)` in the tab's buffer samples and the setting's
  own units, and whether it is on. The lanes are part of the tool's draft
  (`DraftSnapshot`), so every point added or dragged is a draft step, Esc
  throws them away with the rest, and the preview follows them as it follows
  any setting. They move with the audio through an edit's `TimeMap`. A tool's
  Reset clears its lanes; an emptied EQ band slot loses its lanes. They are
  not saved in a session -- nor are the Gain and Pitch Shift curves.
- **Between and beyond the points.** Values are interpolated on the
  setting's scale -- frequencies, Qs, times and the ratio by ratio (a
  straight line on a log axis), the rest linearly -- and hold before the
  first point and after the last.
- **Rendering.** `ToolOp::Eq / Compressor / Limiter / Clipper / Reverb` carry `Tracks` beside
  their settings (in the preview key, so a moved point re-renders). The DSP
  re-evaluates the settings every `AUTOMATION_BLOCK` (32) samples: the
  compressor its coefficients, the limiter its drive and ceiling where a peak
  is judged and where the output is written, the EQ the sections of the
  automated bands, keeping each filter's state. With no tracks every
  processor runs exactly as before. A range render tells the DSP the buffer
  sample its segment starts at, so a curve applies where the buffer has it.
- **Editing.** The Inspector's Automation section picks the setting the
  canvas edits and switches its curve on; the canvas then edits that lane
  like the gain curve (double-click to add or remove a point, drag to move
  it). Other lanes of the tool that are on are drawn faintly, with their names.
- **The panel at the playhead.** A setting that follows a curve shows its
  value at the playhead, read-only, in place of its control. The EQ graph,
  the compressor's transfer curve and the limiter's meters show the settings
  at the playhead; what is dragged on a graph goes back into the settings
  only for what no curve owns (`merge_unautomated`). The time graphs draw an
  automated threshold or ceiling as the curve it is.

## Undo / Redo

Ctrl+Z, Ctrl+Y and Ctrl+Shift+Z act on **the workspace in front**
(`undo_target`): Multi Edits, the Effect Graph, the editor, or the list. In
the editor, in order:

1. An apply or Revert to Saved still running on the tab: refused with a toast.
2. The Spatial view: its own history.
3. A Pencil draft: its stroke history.
4. **The tool's draft history** -- one step per gesture (a drag, a typed
   value, a checkbox, a canvas stroke). Esc discards the whole draft.
5. The tab's history. Only now is a preview cleared.

Nothing is touched when there is nothing to undo. Starting a new draft clears
the tab's redo.

- **One gesture, one step.** Loop edges, loop fields, the seam, marker times
  and labels open `annotation_undo` when the gesture starts and push once when
  it ends (`editor_settle_annotation_gesture`), only if something changed.
- **Markers and loop points are kept as they are made.** No Apply; the old
  `*_committed` / `*_applied` layers follow every edit
  (`editor_sync_annotation_layers`).
- **Unsaved.** `EditorTab::audio_rev` / `saved_audio_rev` (one process-wide
  counter, never reused) decide `dirty`. Undo after a save is unsaved again;
  redo back to the saved state is clean.
- **Revert to Saved** (Edit menu, confirmed) re-reads the file and pushes the
  edited audio as the "Revert to Saved" step: Ctrl+Z brings it back.
- **Long clips.** The step just pushed is never evicted by the byte budget;
  snapshots sharing one buffer count it once.

## Keys

| Key | Action |
|---|---|
| Ctrl+Enter | Apply the active tool (`EditorApplyTool`, rebindable) |
| Esc | Discard the tool's adjustments and its preview |
| Ctrl+Z / Ctrl+Y / Ctrl+Shift+Z | Undo / Redo (see above) |

## Tests

`tests/inspector_workflow.rs` covers the contract above end to end;
`tool_render.rs`, `tool_session.rs` and `editor_tool.rs` have unit tests for
the rules.
