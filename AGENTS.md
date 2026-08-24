# AGENTS.md

Purpose
- Notes for LLM agents and humans working in this repo.
- Focus on cargo workflows, console usage, and core implementation principles.

Terminology
- "Session" (.nwsess) is the current state file name used in UI/docs.
- Legacy code/file naming still uses `project*` to mean session persistence.

Repository Layout
- `assets/licenses/`: data behind the in-app Licenses window. `third_party.json` is the generated, committed snapshot the binary embeds; `extra.json` is the hand-maintained half (bundled C/C++ sources, installer DLLs, fonts, embedded data, runtime-downloaded models, Steinberg VST 3) plus the per-crate flags and notes; `texts/` holds licence texts cargo-about cannot find. Regenerate with `commands/generate_licenses.ps1` after any dependency change -- never hand-edit `third_party.json`.
- `commands/`: PowerShell helper scripts (e.g., Whisper model download, SRT generation, installer build, licence snapshot regeneration).
- `debug/`: Debug fixtures and automation outputs (e.g., gui_test audio, summary text).
- `docs/`: Design/refactor plans and specs.
  - `REFACTOR_PLAN.md`: app.rs / logic.rs refactor plan and progress map.
  - `MAJOR_UPDATE_PLAN.md`: feature roadmap and UX requirements.
  - `NWPROJ_PLAN.md`: session save format/spec (legacy "project" naming in code).
  - `SOUND_LIST_IMPORT_PLAN.md`: CSV/Excel import design and performance notes.
  - `CLIPBOARD_HOTKEY_ISSUE_20260201.md`: clipboard hotkey investigation log.
- `icons/`: App icon sources/exports.
- `installer/`: Installer assets/scripts (packaging).
- `screenshots/`: UI capture output (manual or automated).
- `src/`: Rust sources (app + engine).
  - `src/app/`: WavesPreviewer implementation split by feature.
    - `ui/`: UI panels/windows (top bar, list, editor, debug, export settings).
      - `ui/topbar/`: top bar sections (`menus`, `transport`, `status`).
      - `ui/list/`: list focus/keyboard and table-building helpers.
    - `render/`: waveform/spectrogram rendering helpers.
    - `*_ops.rs`: operation logic split by domain (input, clipboard, session, loading, editor apply, loudnorm, resample, meta, preview, export, external load).
    - `app_init.rs`: startup/build orchestration for `WavesPreviewer`.
    - `perf_profile.rs`: machine performance tier (Low/Normal/High) and every UI-thread budget derived from it — list sort/filter thresholds, frame budget, worker-pool sizes. Read a budget from here rather than adding a constant.
    - `frame_budget.rs`: the shared per-frame deadline the deferrable drains in `frame_ops.rs` consult.
    - `path_status.rs`: background "does this path exist" service. UI code asks this, never the filesystem.
    - `frame_ops.rs`: per-frame `eframe::App::update` orchestration.
    - `tab_ops.rs`: open/activate tab helpers.
    - `editor_decode_ops.rs`: background editor decode spawn/drain helpers.
    - `logic.rs`: per-frame update logic.
    - `types.rs`: shared app state and enums.
    - `project.rs`: session (nwsess) serialization helpers (legacy naming).
    - `session_ops.rs`: session open/save/IPC/drag-drop.
    - `theme_ops.rs`: theme + prefs load/save.
    - `scan_ops.rs`: folder scan job orchestration + results apply.
    - `transcript_ops.rs`: transcript seek handling.
    - `cli_ops.rs`: `--cli` headless command handlers and JSON/render helpers.
    - `gain_ops.rs`: unified per-file gain framework: pending gain lookup/set for list items, plus routing list gain changes into open editor tabs as destructive edits (and baking pending gain on tab open).
    - `list_state_ops.rs`: list accessors, selection helpers, and sort-key visibility guard.
    - `temp_audio_ops.rs`: clipboard temp wav export + virtual audio decode helpers.
    - `rename_ops.rs`: rename dialogs + path replacement and batch rename.
    - `audio_ops.rs`: output volume + per-file gain application.
    - `video_ops.rs`: one decode worker per open video tab, the read-ahead ring that keeps the picture on the playhead rather than a round trip behind it, and the per-frame drain.
    - `licenses.rs`: parses the embedded `assets/licenses/third_party.json` once on first open, pools licence texts by key, and groups flagged entries by topic so one issue spread across a wrapper crate, its `-sys` crate and the C library it builds is stated once. The snapshot is generated against the widest feature set, so `feature_active()` resolves each component's cargo feature with `cfg!` and the window reports the binary in front of the reader rather than claiming obligations for code that was never linked in -- add a feature there whenever `extra.json` gains one. `ui/licenses.rs` renders it as Help -> Licenses.
  - `src/bin/`: extra binaries/utilities (if present).
  - `src/main.rs`: native startup entry.
  - `src/cli.rs`: CLI arg parsing and startup config helpers.
  - `src/lib.rs`: crate entry.
  - `src/audio*.rs`, `src/wave.rs`, `src/markers.rs`, `src/loop_markers.rs`: audio I/O and DSP utilities.
  - `src/media_kind.rs`: whether a path is audio or video, and what the app is allowed to do with it (edit / export / write metadata). Every gate that refuses an action on a video source reads one of these predicates rather than comparing extensions, so making video editable later is a change to this file alone.
  - `src/video/`: video container demux and frame decoding, for preview only — the mini meter picture and the list thumbnail. `container.rs` demuxes ISO-BMFF through the `mp4` crate already used for m4a, `annexb.rs` converts AVCC samples for a raw-bitstream decoder, `frame.rs` rotates and downscales on the worker, and the two backends are Media Foundation (Windows) and OpenH264 (everywhere).
  - `src/ipc.rs`: IPC message definitions.
  - `src/ui_wake.rs`: process-wide handle for waking the UI thread from a background thread (the frame loop sleeps when idle, so a thread pushing into a channel the UI polls must ask for a frame).
  - `src/kittest.rs`: kittest feature helpers.
- `tools/gen-licenses/`: standalone crate that merges `cargo about generate --format json` with `assets/licenses/extra.json` into the committed snapshot. Deliberately outside the main crate so regenerating licences does not need NeoWaves's native build deps (ALSA, X11/Wayland, a C++ toolchain).
- `tests/`: integration tests (including kittest harness).
- `target/`: Cargo build artifacts (generated).

Cargo Features
- `default = glow + plugin_native_vst3 + plugin_native_clap + mp3_lame`.
- `video` (OpenH264 H.264 preview) is deliberately NOT default: the crate compiles Cisco's sources, and Cisco only covers AVC patent fees for users of *their* prebuilt binaries. The released Windows installer gets its video from Media Foundation instead, so nothing is lost. Build with `--features video` for the picture on Linux/macOS.
- AAC encode/decode is intentionally unsupported: neither FDK nor Symphonia's AAC codec is in the graph. `mp3_lame` gates LAME MP3 export; MP3 decoding remains available through Symphonia. `wave::export_format_is_available` is the single predicate every format picker consults.
- A build with no copyleft at all: `cargo build --no-default-features --features glow,plugin_native_vst3,plugin_native_clap`.
- LAME 3.100 is built as a replaceable `libmp3lame.dll` from `vendor/lame-3.100`; `src/lame.rs` is the MIT FFI and the installer must copy the DLL. ONNX Runtime, Oniguruma and SQLite remain linked into the executable. `licenses::tests::redistributed_runtime_dlls_match_the_installer` keeps the licence data honest.

Console Quick Start (PowerShell)
- Build: `cargo build`
- Run: `cargo run`
- Check: `cargo check`
- Tests: `cargo test`
- Release build: `cargo build --release`

CLI Arguments / Modes
- `neowaves` or `cargo run`: GUI startup
- `neowaves --help`: GUI startup flags + `--cli` entry
- `neowaves --cli --help`: headless CLI command tree

Legacy GUI flags
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
- `--help` / `-h`

Headless CLI examples
- `--cli session inspect --session <session.nwsess>`
- `--cli list query --folder <dir>`
- `--cli editor inspect --input <audio>`
- `--cli render waveform --input <audio> --output <png>`
- `--cli render spectrum --input <audio> --output <png>`
- `--cli render editor --input <audio> --output <png>`
- `--cli render list --folder <dir> --output <png>`
- `--cli export file --input <audio> --output <audio>`

Useful Scripts
- `commands\\download_whisper.ps1` (model download)
- `commands\\generate_srt.ps1` (transcript utility)
- `commands\\build_installer.ps1` (installer build)

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
- Size per-frame work from `perf_profile.rs`, not from a new constant: the same number that is fine on an 8-core workstation is seconds of frozen window on a 2-core laptop.
- A new per-frame drain belongs behind the `deferrable!` guard in `frame_ops.rs` unless the user is synchronously waiting on it, and needs a cap on how much it applies per frame.
- **Never call the filesystem from the UI thread** — no `is_file`, `exists`, `metadata`, `read`, or `walkdir` on any path the user supplied. On a network share one of those blocks for the SMB timeout, which is a hung window on its own; a per-frame budget does not help. Ask `path_status.rs` for existence, and put anything else on a worker. (Paths the app owns — its own prefs and config — are the only exception.)
- Background sweeps against a user path must back off from their own measured cost, and check `perf_profile.rs` for whether the root is remote before choosing a concurrency.
- Preserve original files unless the user explicitly saves destructive edits.
- Video sources are read-only: there is no video encoder here, so an edit or an export has nowhere to go. Ask `src/media_kind.rs` rather than testing the extension.
- Prefer progressive loading for long audio (preview first, full decode later).

When Changing Audio/Editor Logic
- Update both waveform visuals and playback buffers.
- If adding background work, wire progress + cancel and log to Debug.
- For large clips, consider using file-based preview paths to avoid UI stalls.

Current staged large-file exceptions
- `src/app/ui/editor.rs`: still the largest UI surface; split by canvas/timeline/tool-panel responsibilities in stages.
- `src/app/ui/effect_graph.rs`: keep behavior stable while peeling canvas/input/inspector helpers apart.
- `src/app/effect_graph_ops.rs`: large but cohesive runtime; split by validation / runner / drain paths instead of arbitrary slices.
