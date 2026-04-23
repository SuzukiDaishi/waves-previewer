---
name: gui-screenshot-debug
description: Use this skill when the user asks to verify NeoWaves GUI behavior by operating the app, taking screenshots, visually inspecting the screenshots yourself, and iterating on fixes. Trigger for screenshot-driven UI debugging, before/after image evidence, trackpad/keyboard/editor interaction checks, or requests like "screenshot -> operation -> screenshot -> confirm".
---

# GUI Screenshot Debug

Use this skill when visual behavior matters. Do not treat a generated PNG, file size, or pixel diff as sufficient proof. Open the screenshots and inspect what changed.

## Goal

Run a repeatable loop:

1. Capture a baseline screenshot.
2. Perform the target operation.
3. Capture an after screenshot.
4. Open both screenshots and visually inspect them.
5. Fix issues if found.
6. Re-run the same screenshot sequence and report evidence.

## Tooling Choices

- Prefer `kittest_render` when the operation can be automated in-process and needs reliable before/after screenshots.
- Prefer `neowaves.exe --cli render editor|list|waveform|spectrum` when a headless render is enough.
- Prefer GUI screenshot flags when checking startup, shell-open, or full-window behavior:

```powershell
neowaves.exe --open-file C:\audio\sample.wav --screenshot C:\work\before.png --screenshot-delay 24 --exit-after-screenshot --no-ipc-forward
```

If NeoWaves is not installed system-wide, use:

```powershell
.\target\release\neowaves.exe ...
```

## Required Procedure

1. Create a stable artifact directory, usually `debug\screenshot_verify\`.
2. Capture the first screenshot before the operation.
3. Apply the operation through kittest, CLI, debug automation, or GUI startup flags.
4. Capture the next screenshot after the operation.
5. Open the screenshots with the available image viewer tool before making a claim.
6. Record concrete visual observations: visible labels, selected tab/tool, waveform position, navigator rectangle, marker/loop overlays, playhead position, or changed panel state.
7. Use pixel diff, image dimensions, file size, logs, and state assertions only as supporting evidence.
8. If the screenshot is visually ambiguous, change the fixture or view so the target behavior is obvious. For waveform pan/zoom, use audio with visible amplitude changes instead of a uniform sine wave.
9. If a defect is found, patch the code, then repeat the same screenshots and compare again.

## Kittest Render Pattern

Use this when the operation is an editor/list interaction:

```powershell
$env:TEMP = (Resolve-Path debug\screenshot_verify)
$env:TMP = $env:TEMP
$env:CARGO_TARGET_DIR = 'target\codex-screenshot-verify'
cargo test --features kittest_render --test gui_kittest_suite <test_name> -- --nocapture
```

The test should save explicit before/after PNG files into the artifact directory. After the test, list the generated PNG paths and open the relevant images.

## CLI Render Pattern

Use this when session or file state is enough:

```powershell
neowaves.exe --cli render editor --input C:\audio\sample.wav --output C:\work\editor_before.png
neowaves.exe --cli render editor --session C:\work\sample.nwsess --path C:\audio\sample.wav --output C:\work\editor_after.png
```

For list checks:

```powershell
neowaves.exe --cli render list --folder C:\audio --output C:\work\list.png
```

## What To Report

- Screenshot paths opened and inspected.
- What operation was performed between screenshots.
- What changed visually, in concrete UI terms.
- Whether the visual result matches the expected behavior.
- Any state/test/log evidence that supports the visual check.
- If fixed, the file changes and the rerun evidence.

## Failure Rules

- Do not say "confirmed by screenshot" unless you opened the image and can describe what is visible.
- Do not rely only on changed pixel counts.
- Do not keep using a fixture whose waveform makes the behavior hard to see.
- If screenshot capture succeeds but the visual state is wrong or unclear, treat the verification as incomplete and iterate.

## Read Next

- Read `../../../docs/CLI_COMMAND_REFERENCE.md` for render and debug screenshot commands.
- Read `../../../docs/CLI_AGENT_WORKFLOWS.md` for larger agent workflows that need visual evidence.
