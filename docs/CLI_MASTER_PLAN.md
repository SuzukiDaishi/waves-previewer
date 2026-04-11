# CLI Master Plan

## Goal

NeoWaves keeps its GUI as the default user experience, but also gains a first-class CLI mode.

- `neowaves` starts the GUI.
- `neowaves --cli ...` runs headless commands.
- CLI is the replacement for MCP automation.
- CLI stdout is machine-readable JSON.
- Image-producing commands save PNG files and return absolute paths.

This document is the source of truth for the rollout order and the steady-state architecture.

## Principles

- GUI is not removed.
- Existing GUI startup flags stay supported.
- New automation capability is added only under `--cli`.
- MCP runtime, flags, menus, and scripts are removed after the equivalent CLI surface exists.
- CLI commands are non-interactive by default and safe for scripting.
- `stdout` is reserved for JSON output. Human-readable logs go to `stderr`.
- Stateful editing remains non-destructive until explicit save/export commands run.

These choices follow the repo's non-destructive editing model and the Unix-style CLI guidance referenced during planning.

## Invocation Model

### GUI mode

Default invocation:

```powershell
neowaves
neowaves --open-file demo.wav
neowaves --open-session work.nwsess
```

Behavior:

- Starts the GUI window.
- Accepts the existing startup/debug flags.
- Does not require a subcommand.

### CLI mode

Headless invocation:

```powershell
neowaves --cli list query --folder .\assets\audio
neowaves --cli editor inspect --input .\demo.wav
neowaves --cli render waveform --input .\demo.wav
```

Behavior:

- Requires a subcommand tree after `--cli`.
- Returns JSON on stdout.
- Uses exit code `0` for success and non-zero for failure.

## Command Tree

Phase 1 command tree:

```text
neowaves --cli session {new,inspect}
neowaves --cli item {inspect,meta,artwork}
neowaves --cli list {columns,query,render}
neowaves --cli editor {inspect,view,selection,markers,loop}
neowaves --cli render {waveform,spectrum,editor,list}
neowaves --cli export {file}
neowaves --cli debug {summary,screenshot}
```

Phase 2 adds:

```text
neowaves --cli external ...
neowaves --cli transcript ...
neowaves --cli music-ai ...
```

Phase 3 adds:

```text
neowaves --cli plugin ...
neowaves --cli effect-graph ...
```

## Output Contract

All CLI commands return the same envelope:

```json
{
  "ok": true,
  "command": "render waveform",
  "result": {},
  "warnings": [],
  "errors": []
}
```

Rules:

- `ok` is `true` only on successful completion.
- `command` is a stable command identifier.
- `result` contains command-specific structured output.
- `warnings` contains non-fatal issues.
- `errors` contains user-facing error strings when `ok` is `false`.
- Paths returned in JSON are absolute paths.

## Architecture

The final architecture has three layers:

1. Root CLI parser
   - Decides between default GUI mode and `--cli`.
   - Owns global help text.
2. CLI command layer
   - Maps clap subcommands to typed command structs.
   - Validates arguments.
   - Emits JSON envelopes.
3. Headless service layer
   - Reads audio/session metadata.
   - Loads list/editor state without relying on live UI interactions.
   - Produces waveform/spectrum/list/editor images.
   - Reuses existing audio/session/export logic where practical.

GUI remains on the existing `WavesPreviewer` path.

## Delivery Order

### Phase 0: Docs First

- Add CLI spec docs.
- Lock command names, help policy, and JSON conventions.
- Update repo docs to point to the new CLI spec.

### Phase 1: Root Parser and Core CLI

- Replace the manual parser with `clap`.
- Keep GUI startup flags working.
- Add `--cli`.
- Implement `session`, `item`, `list`, `editor`, `render`, `export`, and `debug`.
- Add rich layered help.

### Phase 2: MCP Removal

- Remove MCP flags.
- Remove MCP runtime modules and app wiring.
- Remove MCP menu entries from the GUI.
- Remove `commands/mcp_smoke.ps1`.
- Archive old MCP design docs.

### Phase 3: Extended Domains

- Add `external`, `transcript`, and `music-ai`.
- Add `plugin` and `effect-graph`.

## Compatibility

- `.nwsess` remains the session format.
- Existing startup flags remain GUI-only compatibility flags.
- Root positional file/folder/session arguments continue to open the GUI.
- CLI state-changing commands must preserve current loop/marker semantics.
- Export behavior remains non-destructive by default and follows the current overwrite/new-file policy.

## Acceptance Criteria

- `cargo run -- --help` explains both GUI and CLI usage.
- `cargo run -- --cli --help` shows the CLI command tree.
- Phase 1 CLI commands work without MCP.
- GUI can still open files, folders, and sessions with legacy flags.
- No runtime MCP code remains once the migration finishes.
