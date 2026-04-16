---
name: list-query-review
description: Use this skill when the user wants to inspect, filter, sort, select, save queries, or render list evidence with NeoWaves CLI. Trigger for requests about searching audio sets, reviewing list rows, choosing targets by query, or producing a list image before later batch or editor work.
---

# List Query Review

Use `neowaves.exe` for all commands.

If NeoWaves is not installed system-wide, run:

```powershell
.\target\release\neowaves.exe --cli ...
```

## Goal

Inspect list state in a repeatable order:

1. Create or open a session
2. Query rows
3. Sort or search
4. Save a stable query
5. Select the target row if needed
6. Render the list when visual review matters

## Workflow

```powershell
neowaves.exe --cli session new --folder C:\Game\Audio --output C:\work\audio.nwsess
neowaves.exe --cli list query --session C:\work\audio.nwsess --columns file,folder,length,lufs,gain,wave
neowaves.exe --cli list sort --session C:\work\audio.nwsess --sort-key lufs --sort-dir asc
neowaves.exe --cli list search --session C:\work\audio.nwsess --query _BGM
neowaves.exe --cli list save-query --session C:\work\audio.nwsess --query _BGM --sort-key lufs --sort-dir asc
neowaves.exe --cli list select --session C:\work\audio.nwsess --query _BGM --index 0
neowaves.exe --cli render list --session C:\work\audio.nwsess --columns file,lufs,gain,wave --output C:\work\list_review.png
```

## Defaults

- Prefer `list save-query` when the same target set will feed `batch`, `transcript`, or `music-ai`.
- Prefer `render list` when a human needs to review the filtered set.

## Read Next

- Read `../../../docs/CLI_COMMAND_REFERENCE.md` for `list` details.
- Read `../../../docs/CLI_AGENT_WORKFLOWS.md` for list-driven workflows.
