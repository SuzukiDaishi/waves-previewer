# Playback Transport Investigation 2026-03-16

## Goal

Clarify which audio paths must stay offline and which path may use immediate exact-stream playback.

## Current Principle

- Dry pristine physical WAV may use exact-stream playback for immediate editor/list playback.
- Exact-stream is allowed only in `Speed` mode and only when the source has no edits, no preview overlay, no SR/bit-depth override, no per-file gain, and no other processed/virtual source state.
- Master output volume remains realtime.
- All sample-changing work stays offline:
  - sample-rate conversion
  - PitchShift
  - TimeStretch
  - VST/CLAP preview/apply
  - per-file gain
  - edited/virtual audio playback

## Findings

- The previous offline-only branch removed `set_streaming_wav_path()` from runtime playback and forced `Finalizing exact audio` to complete before editor playback.
- That branch fixed some callback complexity, but it also regressed open-to-play latency badly for pristine WAV.
- `dbf97fe`-style behavior was still the right baseline for responsiveness:
  - immediate exact-stream transport for dry WAV
  - full exact buffer/waveform build in the background
  - processed audio kept on offline-rendered buffers

## Implemented Direction

- `src/audio.rs`
  - restored `MappedWavSource`
  - restored `set_streaming_wav_path()`
  - restored exact-stream callback branch
  - kept callback audible work limited to master volume plus transport rate correction
- `src/app.rs`
  - playback session now records both `PlaybackSourceKind` and `PlaybackTransportKind`
  - callback rate is transport-aware:
    - `Buffer` -> `1.0`
    - `ExactStreamWav` -> `user_speed * source_sr / out_sr`
  - source-time seek/restore is transport-aware
- `src/app/logic.rs`
  - editor exact-stream eligibility restored for pristine physical WAV
  - final decode no longer forces a live stream -> buffer swap when the source remains eligible
  - list explicit play/autoplay now prefers exact-stream before heavy offline processing when the source is eligible
  - passive list selection still uses cached/offline preview buffers
- `src/app/audio_ops.rs`
  - master output volume remains realtime
  - per-file gain forces buffer rebuild and disables exact-stream eligibility

## Validation

- `cargo check`
- `cargo test --lib audio::tests -- --nocapture`
- `cargo test --test mp3_preview_timing -- --nocapture`
- `cargo test --features kittest --test small_fix_regressions -- --nocapture`

## Remaining Constraints

- Exact-stream is intentionally narrow. It is not a general realtime DSP path.
- If the output device cannot open the source sample rate directly, exact-stream still relies on callback rate correction.
- Processed audio may still incur render latency; that is expected and preferred over reintroducing callback-side sample-changing DSP.
