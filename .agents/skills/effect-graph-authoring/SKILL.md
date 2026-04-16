---
name: effect-graph-authoring
description: Use this skill when the user wants to create, edit, validate, render, test, or save a NeoWaves effect graph, especially for channel-routing tasks such as mono or stereo to 5-channel conversion. Trigger for requests that require graph structure changes, output validation, or graph image evidence.
---

# Effect Graph Authoring

Use `neowaves.exe` for all commands.

If NeoWaves is not installed system-wide, run:

```powershell
.\target\release\neowaves.exe --cli ...
```

## Goal

Author an effect graph with a repeatable flow:

1. Create or inspect the graph
2. Add or modify nodes
3. Connect edges
4. Validate
5. Render and test
6. Save

## Workflow

### 1. Start from a named graph

```powershell
neowaves.exe --cli effect-graph new --name convert_2ch_to_5ch
neowaves.exe --cli effect-graph inspect --graph convert_2ch_to_5ch
```

### 2. Build the structure

Use `node add`, `node set`, `edge connect`, and `edge disconnect`.

```powershell
neowaves.exe --cli effect-graph node add --graph convert_2ch_to_5ch --kind gain --node-id left_bus --x 220 --y 80
neowaves.exe --cli effect-graph edge connect --graph convert_2ch_to_5ch --from input:out --to left_bus:in
```

Prefer readable `node-id` values. For routing work, use names that reveal channel intent.

### 3. Validate before claiming success

```powershell
neowaves.exe --cli effect-graph validate --graph convert_2ch_to_5ch --report C:\work\convert_2ch_to_5ch_validate.md
```

Check machine-readable warnings and errors first.

### 4. Render and test

```powershell
neowaves.exe --cli effect-graph render --graph convert_2ch_to_5ch --output C:\work\convert_2ch_to_5ch.png
neowaves.exe --cli effect-graph test --graph convert_2ch_to_5ch --input C:\audio\stereo_test.wav --output C:\work\convert_2ch_to_5ch_test.png --report C:\work\convert_2ch_to_5ch_test.md
```

For channel conversion tasks, do not stop at validation. Use `test` output to confirm:

- expected output channel count
- no unintended silent outputs
- usable preview render path

### 5. Save

```powershell
neowaves.exe --cli effect-graph save --graph convert_2ch_to_5ch
```

## Specific Guidance for Mono or Stereo to 5ch

- Treat “good” as “all requested outputs produce sound” unless the user gives a stricter spatial design.
- Use `validate` and `test` to prove channel count and non-silent outputs.
- Keep the graph name stable and explicit, for example `convert_2ch_to_5ch`.

## Read Next

- Read `../../../docs/CLI_COMMAND_REFERENCE.md` for the full effect-graph command family.
- Read `../../../docs/CLI_AGENT_WORKFLOWS.md` for the 2ch-to-5ch example flow.
