# Agent / Human CLI Design

## Goal

The NeoWaves CLI should be excellent for two audiences at the same time:

- LLM agents
  - deterministic commands
  - structured observation
  - safe mutation boundaries
  - easy verification
- human operators
  - strong `--help`
  - copy-pasteable examples
  - reviewable images and reports

The CLI is agent-first, but not agent-only.

## Core Principles

### 1. Observe first, mutate second

Every mutable domain should have inspect/query commands before edit commands.

Examples:

- list query before batch apply
- editor inspect before loop or marker mutation
- effect-graph inspect and validate before save or test

### 2. Session-backed mutation

Direct file mutation is intentionally limited.

- `--input` is read-only
- `--session` is required for mutation
- `export` is the explicit boundary where current session state becomes a file

This keeps agent workflows safe and reviewable.

### 3. One command, one result

The CLI should not rely on a daemon or interactive shell state.

- one command
- one JSON result
- one clear success/failure outcome

This keeps retry logic simple for LLM agents and shell automation.

### 4. Images are evidence

Waveforms, spectrograms, list renders, and effect-graph renders should be easy to generate and review.

- commands save PNG files
- JSON returns absolute paths
- humans can open them directly
- agents can pass them into a later review step

### 5. Stable handles matter

Agents need stable identifiers for replaying workflows.

Current handles:

- `query_id`
- `row_id`
- graph `node.id`
- graph `edge.id`

Future work can add richer selection or batch handles, but the rule is the same: avoid ambiguous stateful references.

## What Makes the CLI Good for Agents

- predictable JSON envelope
- explicit range arguments
- explicit overwrite/export flags
- direct verification commands like `export verify-loop-tags`
- render commands that produce evidence paths
- machine-readable validation from effect-graph commands

## What Makes the CLI Good for Humans

- strong `--help`
- examples that match real game-audio tasks
- reports for batch review
- GUI remains available for exploratory work
- CLI remains scriptable when repetition matters

## Required Command Families

### list

Needed for:

- discovery
- filtering
- sorting
- selection
- query handle generation
- review screenshots

### batch

Needed for:

- loudness normalization planning
- batch application into session state
- batch export
- report generation

### editor

Needed for:

- inspect
- cursor movement
- selection
- markers
- loop authoring
- tool parameter edits
- blocking playback

### render

Needed for:

- waveform review
- spectral review
- list review
- editor review

### export

Needed for:

- new-file export
- overwrite export
- loop/marker verification

### effect-graph

Needed for:

- graph creation
- structural edits
- validation
- test execution
- render review

### external

Needed for:

- external CSV/Excel source management
- merge-rule authoring
- resolved row inspection
- unmatched-row review
- external preview rendering

### transcript

Needed for:

- existing SRT inspection
- AI transcript generation
- batch subtitle generation
- explicit SRT export
- model/config management

### music-ai

Needed for:

- beat/downbeat/section analysis
- marker generation from analysis
- stems export
- analysis report generation
- model status and installation

### plugin

Needed for:

- plugin search-path management
- catalog scan/list/probe
- session draft inspection and editing
- headless preview/apply
- parameter review before effect-graph integration

## Human + Agent Workflow Split

The ideal split is:

- GUI for exploratory listening, quick manual edits, and visual scanning
- CLI for repeatable pipelines, verification, batch work, reports, and agent automation

The two modes should share the same non-destructive semantics and export rules.
