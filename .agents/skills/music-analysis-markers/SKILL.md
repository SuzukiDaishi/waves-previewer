---
name: music-analysis-markers
description: Use this skill when the user wants to analyze music structure with NeoWaves CLI, generate beats or sections, turn analysis into markers, or export stems. Trigger for requests about BPM, beat markers, downbeats, sections, stems, or music analysis reports.
---

# Music Analysis Markers

Use `neowaves.exe` for all commands.

If NeoWaves is not installed system-wide, run:

```powershell
.\target\release\neowaves.exe --cli ...
```

## Goal

Handle music analysis with this flow:

1. Inspect current analysis state
2. Check model availability
3. Run analysis
4. Apply beats, downbeats, or sections into markers
5. Export stems if the task asks for them

## Workflow

```powershell
neowaves.exe --cli music-ai inspect --session C:\work\music.nwsess
neowaves.exe --cli music-ai model status
neowaves.exe --cli music-ai analyze --session C:\work\music.nwsess --report C:\work\music_analysis.md
neowaves.exe --cli music-ai apply-markers --session C:\work\music.nwsess --beats --downbeats --sections --replace
neowaves.exe --cli music-ai export-stems --session C:\work\music.nwsess --output-dir C:\work\stems
```

## Defaults

- Prefer `music-ai inspect` first so you do not redo analysis blindly.
- Prefer `--replace` when markers are meant to become the new ground truth.
- Prefer a report path for nontrivial analysis requests.

## Read Next

- Read `../../../docs/CLI_COMMAND_REFERENCE.md` for `music-ai` commands.
- Read `../../../docs/CLI_AGENT_WORKFLOWS.md` for the music analysis workflow.
