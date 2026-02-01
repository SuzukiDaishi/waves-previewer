# AGENTS.md

Purpose
- Notes for LLM agents and humans working in this repo.
- Focus on cargo workflows, console usage, and core implementation principles.

Console Quick Start (PowerShell)
- Build: `cargo build`
- Run: `cargo run`
- Check: `cargo check`
- Tests: `cargo test`
- Release build: `cargo build --release`

CLI Arguments (src/main.rs)
- `--open-session <session.nwsess>`
- `--open-project <project.nwproj>` (legacy)
- `--open-folder <dir>`
- `--open-file <audio>` (repeatable)
- `--open-first`
- `--open-view-mode <wave|spec|mel>`
- `--waveform-overlay <on|off>`
- `--screenshot <path.png>`
- `--screenshot-delay <frames>`
- `--exit-after-screenshot`
- `--dummy-list <count>`
- `--external-dialog`
- `--debug-summary <path>`
- `--debug-summary-delay <frames>`
- `--external-file <path>`
- `--external-dummy <rows>`
- `--external-dummy-cols <count>`
- `--external-dummy-path <path>`
- `--external-dummy-merge`
- `--external-sheet <name>`
- `--external-has-header <on|off>`
- `--external-header-row <n>` (1-based, 0=auto)
- `--external-data-row <n>` (1-based, 0=auto)
- `--external-key-rule <file|stem|regex>`
- `--external-key-input <file|stem|path|dir>`
- `--external-key-regex <pattern>`
- `--external-key-replace <text>`
- `--external-scope-regex <pattern>`
- `--external-show-unmatched`
- `--debug`
- `--debug-log <path>`
- `--auto-run`
- `--auto-run-editor`
- `--auto-run-pitch-shift <semitones>`
- `--auto-run-time-stretch <rate>`
- `--auto-run-delay <frames>`
- `--auto-run-no-exit`
- `--debug-check-interval <frames>`
- `--mcp-stdio`
- `--mcp-http`
- `--mcp-http-addr <addr>`
- `--mcp-allow-path <path>` (repeatable)
- `--mcp-allow-write`
- `--mcp-allow-export`
- `--mcp-readwrite`
- `--help` / `-h`

Useful Scripts
- `commands\\download_whisper.ps1` (model download)
- `commands\\generate_srt.ps1` (transcript utility)
- `commands\\mcp_smoke.ps1` (MCP smoke tests)

Debugging Tips (App UI)
- Debug Window: Tools → Debug Window or `F12`
- Screenshot: Tools → Screenshot or `F9` (saved to OS screenshots folder)
- Use the Debug window’s Input/Processing sections to verify hotkeys and background jobs.

Editor Debug Automation (CLI)
- Full editor sweep with screenshots:
  `cargo run -- --open-file debug\\gui_test_440.wav --auto-run-editor --auto-run-delay 20`
- Screenshots save to the OS screenshots folder; a summary is saved under `debug\\summary_*.txt`.

Implementation Principles
- Keep the list view fast (large file counts must stay responsive).
- Editor can be slower, but must always show progress/feedback and allow cancel.
- Avoid blocking the UI thread; heavy work should run in background tasks.
- Preserve original files unless the user explicitly saves destructive edits.
- Prefer progressive loading for long audio (preview first, full decode later).

When Changing Audio/Editor Logic
- Update both waveform visuals and playback buffers.
- If adding background work, wire progress + cancel and log to Debug.
- For large clips, consider using file-based preview paths to avoid UI stalls.
