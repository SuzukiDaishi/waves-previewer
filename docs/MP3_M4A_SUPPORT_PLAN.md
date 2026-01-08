# MP3/M4A Support Plan (Draft)

Goal: add mp3/m4a input support and make the WAV-only pipeline extensible to other formats.

## Scope Notes / Open Decisions (No External CLI)
- External CLI tools (ffmpeg) are NOT allowed.
- Encoding for non-WAV saves is not currently available in pure Rust. Need a decision:
  - A) Allow mp3/m4a input but export edited audio only as WAV.
  - B) Allow mp3/m4a input and keep audio data intact, but only write metadata (loop markers).
  - C) Integrate native encoders via Rust bindings (LAME/FDK-AAC) without CLI.
- Loop marker semantics must be standardized (internal samples, exclusive vs inclusive).

## Phase 1: Audio I/O Abstraction (Read Path)
1) Introduce a new module (e.g., `src/audio_io.rs`) with a format-agnostic API:
   - `read_audio_info(path) -> AudioInfo { sample_rate, channels, duration }`
   - `decode_audio_mono(path) -> (Vec<f32>, sr)`
   - `decode_audio_multi(path) -> (Vec<Vec<f32>>, sr)`
   - `decode_audio_mono_prefix(path, max_secs)`
2) Use `symphonia` (features: wav, mp3, isomp4, aac, alac) as the unified decoder.
3) Keep WAV-specific chunk helpers in `wave.rs` (smpl read/write), but call them through the new API.

## Phase 2: Loop Markers + Tags
1) Add loop marker interface:
   - `read_loop_markers(path) -> Option<(start, end)>`
   - `write_loop_markers(path, Option<(start, end)>)`
2) Format mapping:
   - WAV: `smpl` chunk (existing).
   - MP3: ID3v2 `TXXX` keys `LOOPSTART` / `LOOPEND` (via `id3` crate).
   - M4A: MP4 freeform tags (via `mp4ameta`).
3) Define and document internal loop semantics (exclusive end recommended).

## Phase 3: App Integration
1) Replace WAV-only decode calls in:
   - `meta.rs` (duration/peak/lufs/thumb)
   - `logic.rs` (list preview, tab open)
   - `app.rs` (heavy processing paths)
2) Extend file filters:
   - Folder scan: wav, mp3, m4a
   - File dialog filters: WAV/MP3/M4A
3) Update loop marker read/write flow in editor to use the new API.

## Phase 4: Save/Export Behavior for Non-WAV
1) Decide on output behavior for mp3/m4a sources (see Open Decisions).
2) If encoding is required without CLI:
   - Integrate native encoders (mp3lame / FDK-AAC) via Rust bindings.
   - Add build configuration for Windows (dll + licensing notes).
3) If metadata-only is chosen:
   - Allow loop marker writes for mp3/m4a while keeping audio data untouched.
4) Update gain export/overwrite flow accordingly.

## Deliverables
- New audio I/O module with tests for WAV/MP3/M4A decoding.
- Updated list/meta pipeline using the unified decoder.
- Loop marker read/write for WAV/MP3/M4A.
- Documented save behavior for non-WAV formats.
