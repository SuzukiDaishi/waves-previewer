---
name: effect-graph-test
description: Use this skill when the user wants to test an existing NeoWaves effect graph with real audio and confirm output channel count, silence, or preview render evidence. Trigger for requests about graph verification, non-silent outputs, graph test reports, or proving that a graph is usable before saving or sharing it.
---

# Effect Graph Test

Use `neowaves.exe` for all commands.

If NeoWaves is not installed system-wide, run:

```powershell
.\target\release\neowaves.exe --cli ...
```

## Goal

Test an existing effect graph against real input and verify that its outputs behave as intended.

## Workflow

Validate first if needed:

```powershell
neowaves.exe --cli effect-graph validate --graph convert_2ch_to_5ch --report C:\work\convert_2ch_to_5ch_validate.md
```

Run the graph test:

```powershell
neowaves.exe --cli effect-graph test --graph convert_2ch_to_5ch --input C:\audio\stereo_test.wav --output C:\work\convert_2ch_to_5ch_test.png --report C:\work\convert_2ch_to_5ch_test.md
```

## What to Check

Use the JSON result and report to confirm:

- output channel count
- per-channel peak values
- `silent_outputs`
- preview render path

For channel-routing work, do not claim success if any required output is silent.

## Defaults

- Prefer `validate` before `test` for graphs that were just edited.
- Prefer a real input fixture that matches the graph’s expected source type.

## Read Next

- Read `../../../docs/CLI_COMMAND_REFERENCE.md` for `effect-graph test`.
- Read `../../../docs/CLI_AGENT_WORKFLOWS.md` for the `convert_2ch_to_5ch` workflow.
