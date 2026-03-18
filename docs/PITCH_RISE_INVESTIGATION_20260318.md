# Pitch Rise Investigation

Date: 2026-03-18

## Symptom

Users reported that playback sometimes sounds sharp or "pitched up", and that the problem seems to appear in time blocks during playback.

## Root Cause

The main playback callback kept the fractional playhead in `f32`:

- exact-stream path
- buffer path when using fractional interpolation

The callback advanced the playhead as:

```rust
pos_f += rate;
```

where `rate` is often a non-integer ratio such as `44100 / 48000 = 0.91875`.

With long playback, storing the running fractional position in `f32` causes precision loss as the absolute playhead grows. That changes the effective sample advance per callback block, which changes audible pitch locally.

## Reproduction Math

A simple simulation of 180 seconds of playback at:

- source SR: `44100`
- output SR: `48000`
- rate: `0.91875`
- callback block: `480` frames

produced this effective-rate spread:

- `f32`: min `0.875`, max `1.0`, block drift `231.17 cents`
- `f64`: min `0.9187499999534339`, max `0.9187500001862645`, block drift `0.0000004387 cents`

This matches the user report of "block-like" pitch changes.

## Fix

Changed the fractional playhead from `AtomicF32` to `AtomicF64` and updated interpolation/remap code to use `f64` end-to-end for the fractional transport position.

Touched areas:

- `src/audio.rs`
  - `SharedAudio.play_pos_f`
  - `MappedWavSource::sample_at_interp()`
  - `AudioEngine::sample_at_interp()`
  - source remap / seek / replace paths
  - callback playback loops
- `src/app/editor_ops.rs`
- `src/app/plugin_ops.rs`
- `src/app/kittest_ops.rs`

## Regression Coverage

Added/updated:

- `audio::tests::fractional_playhead_precision_stays_stable_for_long_exact_stream_runs`

This test simulates 180 seconds of exact-stream stepping and asserts that block-level drift stays effectively zero.

## Validation

Executed:

- `cargo check`
- `cargo test --lib audio::tests -- --nocapture`
- `cargo test --features kittest --test small_fix_regressions -- --nocapture`

## Remaining Notes

This fix addresses the precision-driven pitch rise in the callback transport.

If users still report sharp blocks after this change, the next suspects are:

- source handoff / rebuild paths that re-seek during playback
- stale processing results replacing active playback state
- device-specific WASAPI timing or driver-side resampling behavior
