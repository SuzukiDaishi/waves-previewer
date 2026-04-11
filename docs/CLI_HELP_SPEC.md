# CLI Help Spec

## Objective

Help output must make the split between GUI mode and CLI mode obvious.

The user should understand:

- default invocation opens the GUI
- `--cli` switches to headless command mode
- GUI startup flags still exist
- CLI is organized as subcommands

## Help Layers

### 1. Root help

Command:

```powershell
neowaves --help
```

Must include:

- one-line product description
- GUI default behavior
- the `--cli` entrypoint
- legacy GUI startup flags
- 3 to 6 short examples

Root help structure:

1. Synopsis
2. GUI mode
3. CLI mode
4. Legacy GUI options
5. Examples

### 2. CLI top help

Command:

```powershell
neowaves --cli --help
```

Must include:

- CLI-only synopsis
- command tree
- shared JSON output rule
- path/output conventions
- short examples

### 3. Subcommand help

Command examples:

```powershell
neowaves --cli list query --help
neowaves --cli render waveform --help
neowaves --cli editor playback play --help
neowaves --cli editor tool set --help
```

Must include:

- purpose sentence
- required vs optional arguments
- supported value sets
- 2 to 5 concrete examples
- short output description
- session mutation rules when the command changes `.nwsess`

## Style Rules

- Use plain English command names.
- Keep examples copy-pasteable.
- Prefer explicit option names over positional ambiguity.
- Mention defaults in help text when they matter.
- Do not describe JSON schemas inline in full; summarize the main fields and point to docs.

## Required Root Examples

Root help must show examples equivalent to:

```powershell
neowaves
neowaves --open-file .\demo.wav
neowaves --cli list query --folder .\assets\audio
neowaves --cli editor inspect --input .\demo.wav
neowaves --cli render waveform --input .\demo.wav --output .\out\wave.png
```

## Required CLI Examples

CLI top help must show examples equivalent to:

```powershell
neowaves --cli session inspect --session .\work.nwsess
neowaves --cli list query --folder .\assets\audio --columns file,length,sr
neowaves --cli render list --folder .\assets\audio --output .\out\list.png
neowaves --cli editor playback play --session .\work.nwsess
neowaves --cli export file --session .\work.nwsess --overwrite
```

## Snapshot Policy

The following help outputs must have stable snapshot tests:

- root `--help`
- `--cli --help`
- `--cli list query --help`
- `--cli editor inspect --help`
- `--cli render waveform --help`
- `--cli editor playback play --help`
- `--cli editor tool set --help`
- `--cli export file --help`

## Error Messaging

If the user passes `--cli` without a subcommand:

- exit non-zero
- print CLI top help to stderr
- include a short error line explaining that a CLI subcommand is required

If the user passes an unknown root flag:

- preserve clap's standard unknown-argument error
- do not silently fall back to GUI behavior
