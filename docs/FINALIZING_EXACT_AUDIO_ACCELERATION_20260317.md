# Finalizing Exact Audio Acceleration

Date: 2026-03-17

## Goal

Reduce `Finalizing exact audio` time without regressing playback timing or hybrid exact-stream behavior.

## Chosen Phase

This pass implements the lowest-risk items from
[docs/FINALIZING_EXACT_AUDIO_QUESTION_20260317.md](/c:/Users/zukky/Desktop/waves-previewer/docs/FINALIZING_EXACT_AUDIO_QUESTION_20260317.md):

1. Add a no-op fast path when editor finalization has no resample and no quantize work.
2. Stop resampling channel-by-channel in the main editor/offline render paths.
3. Use one offline multi-channel resampler instance per conversion.
4. Prefer `rubato::FftFixedInOut` for fixed-ratio offline resampling and keep the existing sinc path as fallback.
5. Reserve the full decode accumulation buffer up front when total frame count is known.

## Implemented Changes

### `src/app/logic.rs`

- `process_editor_decode_channels()` now:
  - exits immediately when no finalization work is needed
  - uses `crate::wave::resample_channels_quality()` instead of looping over channels
- `apply_sample_rate_preview_for_path()` now resamples the full channel set in one call
- `render_channels_offline_with_spec()` now resamples the full channel set in one call
- streaming editor decode now uses `try_reserve(total)` for `full_source_channels`

### `src/wave.rs`

- added/used `resample_channels_quality()` as the shared multi-channel offline entry point
- added `FftFixedInOut`-based fixed-ratio offline resample path
- kept the existing `SincFixedIn` path as fallback
- updated `prepare_for_playback_quality()`, `prepare_for_list_preview_quality()`, and `prepare_for_speed_offline_quality()` to use the shared multi-channel path
- added unit tests for:
  - no-op preservation
  - multi-channel output shape/compatibility against the legacy per-channel path

## Measured Result

Representative probe:

- before: [debug/finalize_probe_30s_44100_20260316.txt](/c:/Users/zukky/Desktop/waves-previewer/debug/finalize_probe_30s_44100_20260316.txt)
  - `editor_open_to_final_ms`: `10707.7ms`
  - `editor_decode_finalize_audio_ms`: `10299.8ms`
- after: [debug/finalize_probe_30s_44100_after_fft_20260317.txt](/c:/Users/zukky/Desktop/waves-previewer/debug/finalize_probe_30s_44100_after_fft_20260317.txt)
  - `editor_open_to_final_ms`: `1418.7ms`
  - `editor_decode_finalize_audio_ms`: `964.4ms`

This is roughly:

- `Finalizing exact audio`: about `10.7x` faster
- editor open to final buffer: about `7.5x` faster

## Validation

Executed:

- `cargo check`
- `cargo test --lib wave::tests -- --nocapture`
- `cargo test --features kittest --test small_fix_regressions -- --nocapture`
- `cargo test --test mp3_preview_timing -- --nocapture`
- GUI probe:
  - `cargo run -- --open-file debug\\finalize_probe_30s_44100.wav --auto-run-editor --auto-run-delay 20 --debug --debug-summary debug\\finalize_probe_30s_44100_after_fft_20260317.txt`

## Remaining Work

The current pass accelerates the existing architecture. It does not yet address the larger design items from the question memo:

- keep pristine editor canonical audio at source sample rate instead of device rate
- defer bit-depth quantize to export/apply
- build final editor buffers progressively from decode chunks instead of holding full source plus full final buffers at once
- split editor canonical audio from playback cache more explicitly

Those remain the next candidates if more reduction is needed.
