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
  longer than `LIVE_PREVIEW_SAMPLE_LIMIT` -- a change shows "Settings
  changed" and Refresh renders.
- **Under the playhead.** A finished preview is swapped in where the
  playhead is, mapped through the `TimeMap`, with a short fade back in
  (`PREVIEW_SWAP_FADE_MS`); playback does not stop. Under a pitch/stretch
  playback mode it stops instead (there is no seamless swap into a rendered
  copy). If the tab is not playing, Play installs the preview.
- **A/B** switches between the original and the preview at the same place.
- **Tool switch** drops the preview at the same place; playback goes on.
- Spawning is behind `deferrable!`; installing a finished preview is not.

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
