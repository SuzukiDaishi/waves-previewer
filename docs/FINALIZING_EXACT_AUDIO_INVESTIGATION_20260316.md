# Finalizing Exact Audio Investigation 2026-03-16

## Summary

- The long `Finalizing exact audio` delay was real, but its main cause was the temporary offline-only playback policy.
- That policy blocked pristine WAV playback until the final full-buffer render completed.
- Hybrid exact-stream playback has now been restored for eligible pristine WAV, so immediate playback no longer depends on `Finalizing exact audio`.
- `Finalizing exact audio` still matters for cache/waveform/final buffer preparation and for processed audio paths.

## Historical Findings

- On the offline-only branch, editor playback was blocked by `!active_editor_exact_audio_ready()`.
- `active_editor_exact_audio_ready()` only became true after final decoded channels were ready.
- `set_streaming_wav_path()` had been disabled, so runtime playback could not reuse the old exact-stream path.
- Measured `44.1kHz` pristine WAV files spent most of the wait in full-buffer audio finalization rather than waveform finalization.

## Current Resolution

- Eligible pristine WAV now activates exact-stream immediately while loading continues in the background.
- Final decode completion updates cached tab audio/waveform data without replacing the live exact-stream transport.
- If the tab becomes ineligible later, playback falls back to the prepared buffer with source-time preservation.

## What Still Uses Finalization

- sample-rate override
- bit-depth override
- per-file gain
- PitchShift
- TimeStretch
- VST/CLAP preview/apply
- edited or virtual audio

Those paths still require offline rendering before playback, by design.

## Conclusion

`Finalizing exact audio` is no longer on the critical path for dry pristine WAV playback.

If future latency complaints appear again, check first whether the source was truly exact-stream eligible or whether it had already crossed into an offline-only processed path.
