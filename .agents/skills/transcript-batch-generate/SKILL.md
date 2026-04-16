---
name: transcript-batch-generate
description: Use this skill when the user wants to inspect transcript state, configure transcription, generate subtitles, or export SRT files with NeoWaves CLI. Trigger for requests about batch subtitle generation, existing SRT inspection, model status, or writing SRT output from a session.
---

# Transcript Batch Generate

Use `neowaves.exe` for all commands.

If NeoWaves is not installed system-wide, run:

```powershell
.\target\release\neowaves.exe --cli ...
```

## Goal

Run transcription work in this order:

1. Inspect transcript state
2. Check model availability
3. Configure the run
4. Generate one file or a batch
5. Export explicit SRT files when needed

## Workflow

```powershell
neowaves.exe --cli transcript inspect --session C:\work\voice_lines.nwsess
neowaves.exe --cli transcript model status
neowaves.exe --cli transcript config set --session C:\work\voice_lines.nwsess --language ja --perf-mode balanced --model-variant small --compute-target auto --overwrite-existing-srt on
neowaves.exe --cli transcript generate --session C:\work\voice_lines.nwsess --write-srt --overwrite-existing
neowaves.exe --cli transcript batch generate --session C:\work\voice_lines.nwsess --query _VOICE --write-srt --overwrite-existing
neowaves.exe --cli transcript export-srt --session C:\work\voice_lines.nwsess --path C:\audio\line001.wav --output C:\work\line001.srt
```

## Defaults

- Prefer `transcript model status` before a generate request.
- Prefer `transcript inspect` if the repo may already contain `.srt`.
- Prefer `batch generate` only after config is explicit.

## Read Next

- Read `../../../docs/CLI_COMMAND_REFERENCE.md` for `transcript` flags.
- Read `../../../docs/CLI_AGENT_WORKFLOWS.md` for transcript workflow examples.
