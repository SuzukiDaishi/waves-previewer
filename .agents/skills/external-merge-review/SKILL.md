---
name: external-merge-review
description: Use this skill when the user wants to attach CSV or Excel metadata to a NeoWaves session, configure merge rules, inspect matched or unmatched rows, or render external merge evidence. Trigger for requests about spreadsheet import, merge-key setup, external column review, or unresolved row inspection.
---

# External Merge Review

Use `neowaves.exe` for all commands.

If NeoWaves is not installed system-wide, run:

```powershell
.\target\release\neowaves.exe --cli ...
```

## Goal

Manage external metadata with this fixed flow:

1. Create or open the session
2. Add the CSV or Excel source
3. Inspect merge state
4. Adjust config
5. Review resolved and unmatched rows
6. Render evidence if needed

## Workflow

```powershell
neowaves.exe --cli session new --folder C:\Game\Audio --output C:\work\external_merge.nwsess
neowaves.exe --cli external source add --session C:\work\external_merge.nwsess --input C:\Game\Meta\audio.xlsx --sheet Sheet1
neowaves.exe --cli external inspect --session C:\work\external_merge.nwsess
neowaves.exe --cli external config set --session C:\work\external_merge.nwsess --key-rule stem --visible-columns Category,SubMix,Owner --show-unmatched
neowaves.exe --cli external rows --session C:\work\external_merge.nwsess --include-unmatched
neowaves.exe --cli external render --session C:\work\external_merge.nwsess --output C:\work\external_merge.png
```

## Defaults

- Prefer `external inspect` before changing config.
- Prefer `external rows --include-unmatched` when the user cares about merge coverage.
- Prefer `external render` when merge review must be shared with a human.

## Read Next

- Read `../../../docs/CLI_COMMAND_REFERENCE.md` for `external` options.
- Read `../../../docs/CLI_AGENT_WORKFLOWS.md` for the external merge example.
