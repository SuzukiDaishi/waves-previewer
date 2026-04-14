# CLI Help Spec

## Objective

Help output must make three things obvious:

- default invocation starts the GUI
- `--cli` switches into headless JSON mode
- agent-first commands are still copy-pasteable for humans

## Help Layers

### 1. Root help

Command:

```powershell
neowaves --help
```

Must explain:

- NeoWaves is GUI-first by default
- `--cli` is the headless automation entrypoint
- legacy GUI startup flags still work
- where to find the CLI docs

Root help should include examples for:

```powershell
neowaves
neowaves --open-file .\demo.wav
neowaves --open-session .\work.nwsess
neowaves --cli list query --folder .\assets\audio
neowaves --cli batch loudness plan --session .\work.nwsess --query _BGM --target-lufs -24
neowaves --cli effect-graph list
neowaves --cli external inspect --session .\work.nwsess
neowaves --cli transcript generate --session .\work.nwsess --write-srt
neowaves --cli music-ai analyze --session .\work.nwsess --report .\music_analysis.md
neowaves --cli plugin scan
```

### 2. CLI top help

Command:

```powershell
neowaves --cli --help
```

Must explain:

- command tree
- stdout JSON rule
- stderr diagnostic rule
- session-backed mutation rule
- representative examples from list, batch, editor, render, export, effect-graph, external, transcript, music-ai, and plugin

CLI top help should include examples for:

```powershell
neowaves --cli session inspect --session .\work.nwsess
neowaves --cli list query --session .\work.nwsess --query _BGM
neowaves --cli batch loudness plan --session .\work.nwsess --query _BGM --target-lufs -24
neowaves --cli editor playback play --session .\work.nwsess --selection
neowaves --cli export verify-loop-tags --input .\out\music_interactive.mp3
neowaves --cli effect-graph test --graph convert_2ch_to_5ch --input .\stereo.wav
neowaves --cli external source add --session .\work.nwsess --input .\meta.xlsx
neowaves --cli transcript generate --session .\work.nwsess --write-srt --overwrite-existing
neowaves --cli music-ai apply-markers --session .\work.nwsess --beats --downbeats --sections --replace
neowaves --cli plugin session apply --session .\work.nwsess
```

### 3. Subcommand help

Representative commands:

```powershell
neowaves --cli list query --help
neowaves --cli batch loudness plan --help
neowaves --cli editor cursor set --help
neowaves --cli editor playback play --help
neowaves --cli render waveform --help
neowaves --cli export file --help
neowaves --cli export verify-loop-tags --help
neowaves --cli effect-graph --help
neowaves --cli external --help
neowaves --cli transcript generate --help
neowaves --cli music-ai analyze --help
neowaves --cli plugin session apply --help
```

Each subcommand help should include:

- one-sentence purpose
- required versus optional arguments
- supported value sets where relevant
- 2 to 5 concrete examples
- short explanation of the main JSON fields
- explicit note when the command mutates `.nwsess`

## Style Rules

- Prefer explicit option names over positional arguments.
- Keep examples copy-pasteable in PowerShell.
- Mention defaults in help text when they matter.
- Mention when commands are read-only versus session-backed mutation.
- Avoid embedding full JSON schemas in help output; summarize the important fields and point to docs.
- Prefer task-oriented examples over synthetic parser-only examples.

## Required Snapshot Coverage

Stable help snapshots are required for:

- root `--help`
- `--cli --help`
- `--cli list query --help`
- `--cli batch loudness plan --help`
- `--cli editor cursor set --help`
- `--cli editor playback play --help`
- `--cli render waveform --help`
- `--cli export file --help`
- `--cli export verify-loop-tags --help`
- `--cli effect-graph --help`
- `--cli external --help`
- `--cli transcript generate --help`
- `--cli music-ai analyze --help`
- `--cli plugin session apply --help`

## Error Messaging

If the user passes `--cli` without a subcommand:

- exit non-zero
- print CLI top help to stderr
- include a short error line explaining that a CLI subcommand is required

If the user passes an unknown root flag:

- preserve clap's standard unknown-argument error
- do not silently fall back to GUI behavior

If a mutating command is invoked without `--session`:

- exit non-zero
- clearly state that session-backed mutation is required

If a batch command receives both `--query` and `--query-id` semantics that conflict:

- prefer the explicit validation error over silent fallback
- tell the user which selector was invalid
