# Agent / Human CLI Workflows

## Purpose

This document turns the high-level CLI design into concrete workflows that both humans and LLM agents can follow.

Each workflow uses the same pattern:

1. Discover
2. Inspect
3. Plan
4. Mutate
5. Export
6. Verify

## Workflow A: Normalize `_BGM` to `-24 LUFS`

### Goal

Find `_BGM` files under multiple folders, review them, apply loudness normalization in session state, then export.

### Recommended flow

Create a session:

```powershell
neowaves --cli session new --folder C:\Game\Audio --output C:\work\bgm.nwsess
```

Save a reusable query:

```powershell
neowaves --cli list save-query --session C:\work\bgm.nwsess --query _BGM
```

Inspect the current list:

```powershell
neowaves --cli list query --session C:\work\bgm.nwsess --query _BGM --columns file,folder,length,lufs,gain,wave
```

Plan loudness changes:

```powershell
neowaves --cli batch loudness plan --session C:\work\bgm.nwsess --query _BGM --target-lufs -24 --report C:\work\bgm_plan.md
```

Apply the pending gain into the session:

```powershell
neowaves --cli batch loudness apply --session C:\work\bgm.nwsess --query _BGM --target-lufs -24
```

Render a review image:

```powershell
neowaves --cli render list --session C:\work\bgm.nwsess --columns file,gain,wave --output C:\work\bgm_after.png
```

Export:

```powershell
neowaves --cli batch export --session C:\work\bgm.nwsess --query _BGM --overwrite --report C:\work\bgm_export.md
```

### Evidence expected

- `query_id`
- list rows with `row_id`
- loudness plan rows with `proposed_gain_db`
- review PNG
- batch export report

## Workflow B: Build `convert_2ch_to_5ch`

### Goal

Create and save an effect graph named `convert_2ch_to_5ch`, validate it, test it, and confirm all outputs are active.

### Recommended flow

Create a new graph:

```powershell
neowaves --cli effect-graph new --name convert_2ch_to_5ch
```

Inspect the graph:

```powershell
neowaves --cli effect-graph inspect --graph convert_2ch_to_5ch
```

Add nodes:

```powershell
neowaves --cli effect-graph node add --graph convert_2ch_to_5ch --kind gain --node-id left_bus --x 220 --y 80
neowaves --cli effect-graph node add --graph convert_2ch_to_5ch --kind gain --node-id right_bus --x 220 --y 220
```

Connect edges:

```powershell
neowaves --cli effect-graph edge connect --graph convert_2ch_to_5ch --from input:out --to left_bus:in
neowaves --cli effect-graph edge connect --graph convert_2ch_to_5ch --from input:out --to right_bus:in
```

Render a diagram:

```powershell
neowaves --cli effect-graph render --graph convert_2ch_to_5ch --output C:\work\convert_2ch_to_5ch.png
```

Validate:

```powershell
neowaves --cli effect-graph validate --graph convert_2ch_to_5ch --report C:\work\convert_2ch_to_5ch_validate.md
```

Test with audio:

```powershell
neowaves --cli effect-graph test --graph convert_2ch_to_5ch --input C:\audio\stereo_test.wav --output C:\work\convert_2ch_to_5ch_test.png --report C:\work\convert_2ch_to_5ch_test.md
```

Save:

```powershell
neowaves --cli effect-graph save --graph convert_2ch_to_5ch
```

### Evidence expected

- graph JSON
- validation summary
- graph PNG
- test PNG
- `per_channel_peak_db`
- `silent_outputs`

## Workflow C: Manual + Assist Loop Export

### Goal

Set a good loop range with session-backed editor commands, review it visually, export it, and verify the written loop tags.

### Recommended flow

Create a one-file session:

```powershell
neowaves --cli session new --input C:\audio\music_interactive.wav --output C:\work\music_loop.nwsess
```

Inspect:

```powershell
neowaves --cli editor inspect --session C:\work\music_loop.nwsess
```

Move the cursor:

```powershell
neowaves --cli editor cursor set --session C:\work\music_loop.nwsess --sample 441000 --snap zero-cross
neowaves --cli editor cursor nudge --session C:\work\music_loop.nwsess --samples 256 --snap zero-cross
```

Set a loop:

```powershell
neowaves --cli editor loop set --session C:\work\music_loop.nwsess --start-sample 441000 --end-sample 882000
neowaves --cli editor loop apply --session C:\work\music_loop.nwsess
```

Render the loop range:

```powershell
neowaves --cli render waveform --session C:\work\music_loop.nwsess --loop --show-loop --show-markers --output C:\work\music_loop_review.png
```

Export WAV or MP3:

```powershell
neowaves --cli export file --session C:\work\music_loop.nwsess --output C:\work\music_loop.mp3 --format mp3
```

Verify written tags:

```powershell
neowaves --cli export verify-loop-tags --input C:\work\music_loop.mp3
```

### Evidence expected

- session-backed loop state
- waveform review PNG
- exported file path
- `loop_verification`
- `verify-loop-tags` result

## Agent Checklist

For every automation task, the agent should try to leave behind:

- a session file if state matters
- at least one reviewable image when visuals matter
- at least one report when batch work is involved
- at least one verification step after export or graph test

## Workflow D: External Table Merge Review

### Goal

Attach CSV/Excel metadata to a session, configure merge rules, review the resolved rows, and keep the merge state inside `.nwsess`.

### Recommended flow

Create or reuse a session:

```powershell
neowaves --cli session new --folder C:\Game\Audio --output C:\work\external_merge.nwsess
```

Add a source:

```powershell
neowaves --cli external source add --session C:\work\external_merge.nwsess --input C:\Game\Meta\audio.xlsx --sheet Sheet1
```

Inspect merge state:

```powershell
neowaves --cli external inspect --session C:\work\external_merge.nwsess
```

Adjust the key rule:

```powershell
neowaves --cli external config set --session C:\work\external_merge.nwsess --key-rule stem --visible-columns Category,SubMix,Owner --show-unmatched
```

Review rows:

```powershell
neowaves --cli external rows --session C:\work\external_merge.nwsess --include-unmatched
```

Render a visual review:

```powershell
neowaves --cli external render --session C:\work\external_merge.nwsess --output C:\work\external_merge.png
```

### Evidence expected

- `sources`
- merge `before/after`
- matched and unmatched rows
- review PNG

## Workflow E: Transcript Generate and Export

### Goal

Read existing subtitles when available, generate missing transcript data, and export `.srt` files as a repeatable session-backed workflow.

### Recommended flow

Inspect current transcript state:

```powershell
neowaves --cli transcript inspect --session C:\work\voice_lines.nwsess
```

Check model state:

```powershell
neowaves --cli transcript model status
```

Configure the transcription run:

```powershell
neowaves --cli transcript config set --session C:\work\voice_lines.nwsess --language ja --perf-mode balanced --model-variant small --compute-target auto --overwrite-existing
```

Generate one file:

```powershell
neowaves --cli transcript generate --session C:\work\voice_lines.nwsess --write-srt --overwrite-existing
```

Generate a batch:

```powershell
neowaves --cli transcript batch generate --session C:\work\voice_lines.nwsess --query _VOICE
```

Export an explicit SRT:

```powershell
neowaves --cli transcript export-srt --session C:\work\voice_lines.nwsess --output C:\work\line001.srt
```

### Evidence expected

- transcript `segments`
- `language`
- `completed_paths`
- exported `.srt` paths

## Workflow F: Music Analysis to Marker Authoring

### Goal

Analyze rhythmic structure, write beats/downbeats/sections into markers, and optionally export stems for downstream game-audio work.

### Recommended flow

Inspect current analysis:

```powershell
neowaves --cli music-ai inspect --session C:\work\music.nwsess
```

Check model availability:

```powershell
neowaves --cli music-ai model status
```

Run analysis:

```powershell
neowaves --cli music-ai analyze --session C:\work\music.nwsess --report C:\work\music_analysis.md
```

Apply analysis events into markers:

```powershell
neowaves --cli music-ai apply-markers --session C:\work\music.nwsess --beats --downbeats --sections --replace
```

Export stems if needed:

```powershell
neowaves --cli music-ai export-stems --session C:\work\music.nwsess --output-dir C:\work\stems
```

### Evidence expected

- `estimated_bpm`
- beat/downbeat/section counts
- generated marker count
- report path
- exported stem file set

## Workflow G: Plugin Scan, Draft, Preview, Apply

### Goal

Search installed plugins, inspect one candidate, set it as the active session draft, preview it headlessly, then apply it to edited audio.

### Recommended flow

Refresh the catalog:

```powershell
neowaves --cli plugin scan
```

Search for candidates:

```powershell
neowaves --cli plugin list --filter OTT
```

Probe a plugin:

```powershell
neowaves --cli plugin probe --plugin "C:\Program Files\Common Files\VST3\OTT.vst3"
```

Inspect the current draft:

```powershell
neowaves --cli plugin session inspect --session C:\work\mix.nwsess
```

Set the draft:

```powershell
neowaves --cli plugin session set --session C:\work\mix.nwsess --plugin "C:\Program Files\Common Files\VST3\OTT.vst3"
```

Preview:

```powershell
neowaves --cli plugin session preview --session C:\work\mix.nwsess
```

Apply:

```powershell
neowaves --cli plugin session apply --session C:\work\mix.nwsess
```

### Evidence expected

- catalog entries
- probe `params` and `capabilities`
- session `plugin_fx_draft`
- `preview_overlay_ready`
- apply result with mutated target
