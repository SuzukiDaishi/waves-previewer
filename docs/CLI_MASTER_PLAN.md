# CLI Master Plan

## Goal

NeoWaves keeps its GUI as the default user experience and grows `--cli` into the primary automation surface for LLM agents and human operators.

- `neowaves` starts the GUI.
- `neowaves --cli ...` runs headless commands.
- `stdout` is machine-readable JSON.
- image-producing commands save PNG files and return absolute paths.
- mutating commands are session-backed and non-destructive until export.

Related docs:

- `docs/CLI_AGENT_HUMAN_DESIGN.md`
- `docs/CLI_AGENT_WORKFLOWS.md`
- `docs/CLI_COMMAND_REFERENCE.md`
- `docs/CLI_HELP_SPEC.md`

## Current Position

Implemented now:

- `session`
- `item`
- `list`
- `batch`
- `editor`
- `render`
- `export`
- `effect-graph`
- `external`
- `transcript`
- `music-ai`
- `plugin`
- `debug`

Still in progress:

- broader report-format unification
- deeper plugin/effect-graph session integration
- more task-oriented help/examples

## Principles

- GUI is preserved and remains the default launch mode.
- New automation capability is added under `--cli`; GUI startup flags remain GUI-only compatibility flags.
- `stdout` is reserved for JSON output; human-readable diagnostics belong on `stderr`.
- Images are first-class evidence and are returned as saved PNG paths, not embedded blobs.
- Session-backed commands operate on current session state rather than only source-file baseline metadata.
- Export is the boundary where current session edits become real files.
- `--input` is read-only; mutation belongs to `--session`.

## Command Tree

```text
neowaves --cli session {new,inspect}
neowaves --cli item {inspect,meta,artwork}
neowaves --cli list {columns,query,sort,search,select,save-query,render}
neowaves --cli batch {loudness {plan,apply},export}
neowaves --cli editor {
  inspect,
  view {get,set},
  selection {get,set,clear},
  cursor {get,set,nudge},
  playback {play},
  tool {get,set,apply},
  markers {list,add,set,remove,clear,apply},
  loop {get,set,clear,apply,mode,xfade,repeat}
}
neowaves --cli render {waveform,spectrum,editor,list}
neowaves --cli export {file,verify-loop-tags}
neowaves --cli effect-graph {
  list,new,inspect,render,validate,test,save,import,export,
  node {add,remove,set},
  edge {connect,disconnect}
}
neowaves --cli external {
  inspect,render,rows,
  source {list,add,reload,remove,clear},
  config {get,set}
}
neowaves --cli transcript {
  inspect,
  model {status,download,uninstall},
  config {get,set},
  generate,
  batch {generate},
  export-srt
}
neowaves --cli music-ai {
  inspect,
  model {status,download,uninstall},
  analyze,
  apply-markers,
  export-stems
}
neowaves --cli plugin {
  search-path {list,add,remove,reset},
  scan,list,probe,
  session {inspect,set,preview,apply,clear}
}
neowaves --cli debug {summary,screenshot}
```

## Output Contract

All commands return the same envelope:

```json
{
  "ok": true,
  "command": "batch.loudness.plan",
  "result": {},
  "warnings": [],
  "errors": []
}
```

Rules:

- `command` is a stable dotted identifier.
- `result` is structured and machine-readable.
- warnings are non-fatal.
- errors are user-facing strings when `ok` is `false`.
- returned paths are absolute.

## Session-backed Mutation Model

- `--input` is read-only.
- `--session` is required for mutation.
- `--path` optionally selects a target row inside the session.
- if `--path` is omitted, the active tab is preferred, then the first tab, then the first session item.

This model is critical for:

- batch loudness planning and apply
- loop and marker editing
- tool parameter editing and apply
- external-source configuration
- transcript/music analysis state
- plugin draft editing and apply
- export from current editor state

## Milestone Breakdown

### Milestone 1: Batch Loudness + Loop Export

Completed surface:

- `list sort`
- `list search`
- `list select`
- `list save-query`
- `batch loudness plan`
- `batch loudness apply`
- `batch export`
- `editor cursor`
- session-backed `render waveform`
- session-backed `export file`
- `export verify-loop-tags`

Acceptance goals:

- `_BGM` style folder/query workflows are scriptable
- loop setup, render, export, and verify work without GUI fallback
- batch reports can be emitted as files

### Milestone 2: Effect Graph Hybrid CLI

Completed surface:

- `effect-graph list`
- `effect-graph new`
- `effect-graph inspect`
- `effect-graph render`
- `effect-graph validate`
- `effect-graph test`
- `effect-graph save`
- `effect-graph import`
- `effect-graph export`
- `effect-graph node {add,remove,set}`
- `effect-graph edge {connect,disconnect}`

Acceptance goals:

- graph JSON remains the single source of truth
- graph rendering and validation are machine-readable
- graph test returns enough evidence for agent review

### Milestone 3: Extended Namespaces

Completed surface:

- `external`
- `transcript`
- `music-ai`
- `plugin`

Acceptance goals:

- external table merge and render are session-backed and automatable
- transcript read/generate/export flows are headless
- music analysis can generate markers and stems without GUI
- plugin catalog, probe, draft preview, and apply work from CLI

### Milestone 4: Agent Ergonomics

Partially implemented:

- `query_id`
- `row_id`
- report output on selected workflows

Still to improve:

- broader report format unification
- richer row/selection handles
- more task-oriented examples in help
- human-friendly troubleshooting and workflow docs

## Architecture

The steady-state CLI architecture is:

1. Root parser
   - decides between GUI default mode and `--cli`
2. Typed CLI command layer
   - clap-based subcommands and argument validation
3. Headless execution layer
   - session loading
   - editor/list/query mutation
   - waveform/spectrum rendering
   - export and verification
   - effect-graph validation and testing
   - external merge/configuration
   - transcript/music-ai pipelines
   - plugin catalog/probe/session draft control

GUI remains on the existing `WavesPreviewer` path.

## Immediate Next Work

- unify `--report` behavior across more commands
- improve task-oriented help examples for game-audio pipelines
- normalize returned absolute paths where `..` segments remain
- deepen plugin/effect-graph session integration and verification
