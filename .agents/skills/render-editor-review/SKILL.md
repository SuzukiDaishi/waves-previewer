---
name: render-editor-review
description: Use this skill when the user wants a reviewable editor image from NeoWaves CLI, especially to inspect waveform, spectrogram, overlays, loop state, markers, or zoomed editor context. Trigger for requests to render the editor view as evidence before or after editing decisions.
---

# Render Editor Review

Use `neowaves.exe` for all commands.

If NeoWaves is not installed system-wide, run:

```powershell
.\target\release\neowaves.exe --cli ...
```

## Goal

Produce a reviewable editor PNG that captures the current session or input state.

## Workflow

Render directly from a file:

```powershell
neowaves.exe --cli render editor --input C:\audio\music.wav --output C:\work\editor_review.png
```

Render from a session-backed target:

```powershell
neowaves.exe --cli render editor --session C:\work\music.nwsess --path C:\audio\music.wav --output C:\work\editor_review.png
```

Render a different view mode:

```powershell
neowaves.exe --cli render editor --session C:\work\music.nwsess --path C:\audio\music.wav --view-mode spec --output C:\work\editor_spec.png
```

## Defaults

- Prefer session-backed render when loop, marker, tool, or selection state matters.
- Prefer saving a new PNG after any meaningful edit so the result can be reviewed later.
- If the task requires operating the GUI and comparing screenshots before/after an interaction, use `../gui-screenshot-debug/SKILL.md` instead of relying on a single render.

## Read Next

- Read `../../../docs/CLI_COMMAND_REFERENCE.md` for `render editor`.
- Read `../../../docs/CLI_AGENT_WORKFLOWS.md` when the render is part of a larger loop or graph workflow.
