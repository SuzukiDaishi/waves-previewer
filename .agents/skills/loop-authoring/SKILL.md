---
name: loop-authoring
description: Use this skill when the user wants to set loop points in NeoWaves CLI, refine them with waveform review and zero-cross nudging, and export WAV or MP3 with loop markers. Trigger for requests about loop authoring, loop review images, loop verification, or producing both WAV and MP3 outputs with preserved loop metadata.
---

# Loop Authoring

Use `neowaves.exe` for all commands.

If NeoWaves is not installed system-wide, run:

```powershell
.\target\release\neowaves.exe --cli ...
```

## Goal

Author loop points with a manual-plus-assist flow:

1. Create a session for the target file
2. Inspect and render the waveform
3. Move the cursor and nudge with zero-cross snap
4. Set and apply the loop
5. Re-render the review image
6. Export WAV or MP3
7. Verify loop tags

## Workflow

### 1. Create the session

```powershell
neowaves.exe --cli session new --input C:\audio\music_interactive.wav --output C:\work\music_loop.nwsess
neowaves.exe --cli editor inspect --session C:\work\music_loop.nwsess
```

### 2. Render the current review image

```powershell
neowaves.exe --cli render waveform --session C:\work\music_loop.nwsess --show-loop --show-markers --output C:\work\music_loop_review.png
```

### 3. Position the cursor

Use absolute cursor placement first, then nudge with zero-cross snap.

```powershell
neowaves.exe --cli editor cursor set --session C:\work\music_loop.nwsess --sample 441000 --snap zero-cross
neowaves.exe --cli editor cursor nudge --session C:\work\music_loop.nwsess --samples 256 --snap zero-cross
```

### 4. Set and apply the loop

```powershell
neowaves.exe --cli editor loop set --session C:\work\music_loop.nwsess --start-sample 441000 --end-sample 882000
neowaves.exe --cli editor loop apply --session C:\work\music_loop.nwsess
```

### 5. Re-render for confirmation

```powershell
neowaves.exe --cli render waveform --session C:\work\music_loop.nwsess --loop --show-loop --show-markers --output C:\work\music_loop_review_after.png
```

### 6. Export

```powershell
neowaves.exe --cli export file --session C:\work\music_loop.nwsess --output C:\work\music_loop.wav --format wav
neowaves.exe --cli export file --session C:\work\music_loop.nwsess --output C:\work\music_loop.mp3 --format mp3
```

### 7. Verify loop persistence

```powershell
neowaves.exe --cli export verify-loop-tags --input C:\work\music_loop.wav
neowaves.exe --cli export verify-loop-tags --input C:\work\music_loop.mp3
```

## Defaults

- Prefer saving a review image before and after loop placement.
- Prefer `--snap zero-cross` when nudging.
- Treat `export verify-loop-tags` as mandatory for MP3 loop delivery.

## Read Next

- Read `../../../docs/CLI_COMMAND_REFERENCE.md` for `editor cursor`, `editor loop`, `render waveform`, and `export verify-loop-tags`.
- Read `../../../docs/CLI_AGENT_WORKFLOWS.md` for the loop authoring example.
