---
name: verify-loop-tags
description: Use this skill when the user wants to confirm whether exported WAV or MP3 files actually contain loop markers or loop tag data. Trigger for requests about loop persistence, loop-tag verification, export validation, or confirming that an output file is ready for game runtime use.
---

# Verify Loop Tags

Use `neowaves.exe` for all commands.

If NeoWaves is not installed system-wide, run:

```powershell
.\target\release\neowaves.exe --cli ...
```

## Goal

Verify that an exported file contains the expected loop metadata after export.

## Workflow

Check a WAV:

```powershell
neowaves.exe --cli export verify-loop-tags --input C:\work\music_loop.wav
```

Check an MP3:

```powershell
neowaves.exe --cli export verify-loop-tags --input C:\work\music_loop.mp3
```

## What to Look For

Use the JSON result to confirm:

- loop presence
- loop start and end when available
- format-specific tag or chunk status
- warnings for missing or incomplete loop metadata

## Defaults

- Treat verification as mandatory after MP3 loop export.
- Prefer checking both WAV and MP3 when both outputs were requested.

## Read Next

- Read `../../../docs/CLI_COMMAND_REFERENCE.md` for `export verify-loop-tags`.
- Read `../../../docs/CLI_AGENT_WORKFLOWS.md` for the loop authoring example.
