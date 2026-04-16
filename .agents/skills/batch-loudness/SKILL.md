---
name: batch-loudness
description: Use this skill when the user wants to normalize loudness across many files with NeoWaves CLI, especially for BGM folders, search-filtered sets, or reportable batch export flows. Trigger for requests to align LUFS, preview the plan, apply loudness adjustments in-session, and export the results safely.
---

# Batch Loudness

Use `neowaves.exe` for all commands.

If NeoWaves is not installed system-wide, run:

```powershell
.\target\release\neowaves.exe --cli ...
```

## Goal

Normalize a filtered set of files to a target LUFS using this fixed flow:

1. Build a session from the source folders
2. Save a stable query
3. Plan loudness changes
4. Apply changes into the session
5. Export the filtered set
6. Re-query or inspect exported results if the task needs confirmation

## Workflow

### 1. Create the session

```powershell
neowaves.exe --cli session new --folder C:\Game\Audio\BGM_A --folder C:\Game\Audio\BGM_B --folder C:\Game\Audio\BGM_C --output C:\work\bgm.nwsess
```

### 2. Capture the target set

```powershell
neowaves.exe --cli list save-query --session C:\work\bgm.nwsess --query _BGM --sort-key lufs --sort-dir asc
```

Prefer `query_id` if the result will be reused across multiple commands.

### 3. Review the plan

```powershell
neowaves.exe --cli batch loudness plan --session C:\work\bgm.nwsess --query _BGM --target-lufs -24 --report C:\work\bgm_plan.md
```

Check:

- target count
- current LUFS per file
- estimated gain
- warnings or skips

### 4. Apply to the session

```powershell
neowaves.exe --cli batch loudness apply --session C:\work\bgm.nwsess --query _BGM --target-lufs -24
```

This updates session state only. It should not be treated as file export.

### 5. Export

Choose one:

```powershell
neowaves.exe --cli batch export --session C:\work\bgm.nwsess --query _BGM --overwrite --report C:\work\bgm_export.md
neowaves.exe --cli batch export --session C:\work\bgm.nwsess --query _BGM --output-dir C:\work\out --report C:\work\bgm_export.md
```

### 6. Confirm

Use at least one confirmation path:

```powershell
neowaves.exe --cli list query --session C:\work\bgm.nwsess --query _BGM --columns file,lufs,gain
neowaves.exe --cli render list --session C:\work\bgm.nwsess --columns file,lufs,gain,wave --output C:\work\bgm_after.png
```

## Defaults

- Prefer `plan` before `apply`.
- Prefer `query_id` when the target set will be reused.
- Prefer reports for any multi-folder batch request.

## Read Next

- Read `../../../docs/CLI_COMMAND_REFERENCE.md` for `batch loudness` and `batch export` options.
- Read `../../../docs/CLI_AGENT_WORKFLOWS.md` for the BGM normalization example.
