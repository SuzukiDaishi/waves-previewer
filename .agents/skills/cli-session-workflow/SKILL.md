---
name: cli-session-workflow
description: Use this skill when NeoWaves CLI work spans multiple commands and needs a safe session-backed workflow. Trigger for requests that combine session creation, list filtering, render review, export, verification, or when the user asks how to compose NeoWaves commands to complete an audio task.
---

# CLI Session Workflow

Use `neowaves.exe` for examples and execution.

If NeoWaves is not installed system-wide, run the repo-local binary from the repository root:

```powershell
.\target\release\neowaves.exe --cli ...
```

## Goal

Drive NeoWaves CLI work in a predictable order:

1. Create or open a `.nwsess`
2. Inspect or query the session state
3. Render evidence when review matters
4. Mutate only through `--session`
5. Export explicitly
6. Verify exported loop/tag state when relevant

## Rules

- Prefer `--session` for any mutation.
- Treat `--input` as read-only.
- Keep `stdout` JSON as the source of truth.
- Save images or reports when the task needs reviewable evidence.
- Prefer `query_id` or explicit `--path` over vague row references.

## Core Workflow

### 1. Start a session

Use one of:

```powershell
neowaves.exe --cli session new --folder C:\Game\Audio --output C:\work\audio.nwsess
neowaves.exe --cli session new --input C:\audio\a.wav --input C:\audio\b.wav --output C:\work\audio.nwsess
neowaves.exe --cli session inspect --session C:\work\audio.nwsess
```

### 2. Narrow the target set

Use `list query`, `list sort`, `list search`, or `list save-query`.

```powershell
neowaves.exe --cli list query --session C:\work\audio.nwsess --query _BGM --columns file,folder,lufs,wave
neowaves.exe --cli list save-query --session C:\work\audio.nwsess --query _BGM --sort-key lufs --sort-dir asc
```

### 3. Inspect or render before mutating

Use render commands to create reviewable evidence.

```powershell
neowaves.exe --cli render list --session C:\work\audio.nwsess --columns file,length,wave --output C:\work\list.png
neowaves.exe --cli render waveform --session C:\work\audio.nwsess --path C:\audio\music.wav --output C:\work\wave.png
neowaves.exe --cli render editor --session C:\work\audio.nwsess --path C:\audio\music.wav --output C:\work\editor.png
```

### 4. Mutate session state

Choose the domain-specific command set:

- loudness: use `batch loudness`
- graph authoring: use `effect-graph`
- loop work: use `editor cursor`, `editor loop`
- transcript/music/plugin: use their own namespaces

### 5. Export explicitly

```powershell
neowaves.exe --cli export file --session C:\work\audio.nwsess --overwrite
neowaves.exe --cli export file --session C:\work\audio.nwsess --output C:\work\music_loop.mp3 --format mp3
```

### 6. Verify when persistence matters

```powershell
neowaves.exe --cli export verify-loop-tags --input C:\work\music_loop.mp3
```

## Read Next

- Read `../../../docs/CLI_COMMAND_REFERENCE.md` for exact command arguments.
- Read `../../../docs/CLI_AGENT_WORKFLOWS.md` for end-to-end task examples.
