//! The committed fixture matrix, and what the app must make of it.
//!
//! `test_samples/formats/README.md` tells a reader what each file proves. This
//! is the same table as code, so the README cannot quietly drift from the
//! behaviour: change what the app does to one of these files and this test says
//! so.
//!
//! Regenerate the fixtures with:
//! `cargo run --manifest-path tools/gen-wav-fixtures/Cargo.toml`

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// What the full decoder does with a file.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Decode {
    /// Decodes, yielding this many channels and frames.
    Ok { channels: usize, frames: usize },
    /// Refuses the file. The editor must say so rather than opening on an
    /// empty canvas.
    Fails,
}

use Decode::{Fails, Ok as Decodes};

struct Expect {
    name: &'static str,
    /// `read_wave_pcm_info`: `Some((channels, sample_rate, bits, frames))`, or
    /// `None` where the fast header path declines the file and leaves it to
    /// the decoder.
    fast: Option<(u16, u32, u16, u64)>,
    decode: Decode,
    /// `wave_metadata_requires_sidecar`: markers and loop points cannot be
    /// written back into the file itself.
    sidecar: bool,
}

const fn e(
    name: &'static str,
    fast: Option<(u16, u32, u16, u64)>,
    decode: Decode,
    sidecar: bool,
) -> Expect {
    Expect {
        name,
        fast,
        decode,
        sidecar,
    }
}

/// A plain file: the header path and the decoder agree on everything.
const fn plain(
    name: &'static str,
    channels: u16,
    sample_rate: u32,
    bits: u16,
    frames: u64,
) -> Expect {
    e(
        name,
        Some((channels, sample_rate, bits, frames)),
        Decodes {
            channels: channels as usize,
            frames: frames as usize,
        },
        false,
    )
}

#[rustfmt::skip]
const EXPECTED: &[Expect] = &[
    // --- Channel counts -----------------------------------------------
    // Everything up to 26 channels reads. Symphonia's channel bitmask
    // defines 26 bits, and an extensible header past that cannot name its
    // channels, so 26 and 27 sit either side of a hard ceiling.
    plain("ch_01.wav", 1, 48_000, 24, 48_000),
    plain("ch_02.wav", 2, 48_000, 24, 48_000),
    plain("ch_03.wav", 3, 48_000, 16, 12_000),
    plain("ch_04.wav", 4, 48_000, 16, 12_000),
    plain("ch_06.wav", 6, 48_000, 24, 48_000),
    plain("ch_08.wav", 8, 48_000, 24, 48_000),
    // 9 and 11 have no standard speaker layout (`audio_channels::standard_layout`
    // returns None), so routing falls back to channel index. They still decode.
    plain("ch_09.wav", 9, 48_000, 16, 12_000),
    plain("ch_10.wav", 10, 48_000, 16, 12_000),
    plain("ch_11.wav", 11, 48_000, 16, 12_000),
    plain("ch_12.wav", 12, 48_000, 24, 48_000),
    plain("ch_16.wav", 16, 48_000, 16, 12_000),
    plain("ch_24.wav", 24, 48_000, 16, 4_800),
    plain("ch_26.wav", 26, 48_000, 16, 4_800),
    // Past the ceiling. The list row still fills in from the header, which is
    // why these are worth having: the failure has to surface somewhere.
    e("ch_27.wav", Some((27, 48_000, 16, 4_800)), Fails, false),
    e("ch_32.wav", Some((32, 48_000, 16, 4_800)), Fails, false),

    // --- What the samples contain -------------------------------------
    // Identical headers; only the audio differs. See the README for what each
    // one looks like in the editor.
    plain("content_ch1_only.wav", 12, 48_000, 16, 48_000),
    plain("content_in_phase.wav", 12, 48_000, 16, 48_000),
    plain("content_antiphase.wav", 12, 48_000, 16, 48_000),
    plain("content_per_channel_tones.wav", 12, 48_000, 16, 48_000),

    // --- Bit depths ---------------------------------------------------
    // The whole of the fast path's accepted table:
    // (1,8) | (1,16) | (1,24) | (1,32) | (3,32).
    plain("depth_08bit.wav", 2, 48_000, 8, 24_000),
    plain("depth_16bit.wav", 2, 48_000, 16, 24_000),
    plain("depth_24bit.wav", 2, 48_000, 24, 24_000),
    plain("depth_32int.wav", 2, 48_000, 32, 24_000),
    plain("depth_32float.wav", 2, 48_000, 32, 24_000),

    // --- Sample rates -------------------------------------------------
    plain("rate_008000.wav", 2, 8_000, 16, 4_000),
    plain("rate_011025.wav", 2, 11_025, 16, 5_513),
    plain("rate_016000.wav", 2, 16_000, 16, 8_000),
    plain("rate_022050.wav", 2, 22_050, 16, 11_025),
    plain("rate_032000.wav", 2, 32_000, 16, 16_000),
    plain("rate_044100.wav", 2, 44_100, 16, 22_050),
    plain("rate_048000.wav", 2, 48_000, 16, 24_000),
    plain("rate_088200.wav", 2, 88_200, 16, 44_100),
    plain("rate_096000.wav", 2, 96_000, 16, 48_000),
    plain("rate_176400.wav", 2, 176_400, 16, 88_200),
    plain("rate_192000.wav", 2, 192_000, 16, 96_000),

    // --- Headers no ordinary writer produces --------------------------
    // A channel mask that disagrees with the channel count is repaired rather
    // than rejected: bits are added above the top one, or dropped from it.
    plain("edge_ext_mask_714.wav", 12, 48_000, 16, 4_800),
    plain("edge_ext_mask_zero.wav", 12, 48_000, 16, 4_800),
    plain("edge_ext_mask_too_few.wav", 12, 48_000, 16, 4_800),
    plain("edge_ext_mask_too_many.wav", 12, 48_000, 16, 4_800),
    // An extensible sub-format that is neither PCM nor IEEE float is refused
    // by every layer.
    e("edge_ext_subformat_unknown.wav", None, Fails, false),
    // 64-bit float is outside the fast path's table but inside what the
    // decoder handles, so the proxy overview is skipped and the full decode
    // carries the file. The two paths genuinely disagree here.
    e("edge_float64.wav", None, Decodes { channels: 2, frames: 4_800 }, false),
    // An odd `data` payload needs a pad byte before the next chunk; a reader
    // that forgets it reads the following chunk at the wrong offset.
    plain("edge_data_odd_len.wav", 1, 48_000, 8, 4_801),
    plain("edge_chunks_before_fmt.wav", 2, 48_000, 16, 4_800),
    plain("edge_chunks_after_data.wav", 2, 48_000, 16, 4_800),
    // `data` claims twice the bytes the file holds. The header path clamps to
    // what is actually there; the decoder runs out mid-stream and fails.
    e("edge_data_truncated.wav", Some((2, 48_000, 16, 4_800)), Fails, false),
    // A `data` chunk with nothing in it. The decoder returns no channels at
    // all -- not one empty channel -- which anything downstream has to survive.
    e("edge_data_zero.wav", Some((2, 48_000, 16, 0)), Decodes { channels: 0, frames: 0 }, false),
    // `nBlockAlign` is not a whole number of frames, so the two readers derive
    // different lengths from the same file. Pinned as-is: this is the
    // behaviour, not an endorsement of it.
    e("edge_block_align_mismatch.wav", Some((3, 48_000, 16, 4_114)), Decodes { channels: 3, frames: 4_223 }, false),
    // RF64/BW64: the app's own header reader takes them, the decoder wants a
    // literal `RIFF` marker. Markers and loops are forced into sidecar files.
    e("edge_rf64.wav", Some((2, 48_000, 16, 4_800)), Fails, true),
    e("edge_bw64.wav", Some((2, 48_000, 16, 4_800)), Fails, true),
    // Not a WAV at all.
    e("edge_not_a_wav.wav", None, Fails, false),
];

fn fixture_dir() -> PathBuf {
    std::env::var("NEOWAVES_FORMAT_FIXTURE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| Path::new(env!("CARGO_MANIFEST_DIR")).join("test_samples/formats"))
}

/// The fixtures are committed, so a missing directory is a broken checkout, not
/// a reason to pass quietly.
#[test]
fn every_committed_fixture_has_an_expectation() {
    let dir = fixture_dir();
    assert!(
        dir.is_dir(),
        "fixture directory missing: {}\nregenerate with: cargo run --manifest-path tools/gen-wav-fixtures/Cargo.toml",
        dir.display()
    );
    let on_disk: BTreeSet<String> = std::fs::read_dir(&dir)
        .expect("read fixture dir")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".wav"))
        .collect();
    let expected: BTreeSet<String> = EXPECTED.iter().map(|e| e.name.to_string()).collect();

    let unexpected: Vec<&String> = on_disk.difference(&expected).collect();
    let missing: Vec<&String> = expected.difference(&on_disk).collect();
    assert!(
        unexpected.is_empty(),
        "fixtures on disk with no entry in this table (and so nothing in the README either): {unexpected:?}"
    );
    assert!(
        missing.is_empty(),
        "fixtures named in this table but not on disk: {missing:?}\nregenerate with: cargo run --manifest-path tools/gen-wav-fixtures/Cargo.toml"
    );
}

#[test]
fn the_header_path_reads_what_the_readme_says() {
    let dir = fixture_dir();
    for case in EXPECTED {
        let path = dir.join(case.name);
        let info = neowaves::wav_stream::read_wave_pcm_info(&path)
            .unwrap_or_else(|err| panic!("{}: header read errored: {err}", case.name));
        match (case.fast, info) {
            (Some((channels, sample_rate, bits, frames)), Some(actual)) => {
                assert_eq!(actual.channels, channels, "{}: channels", case.name);
                assert_eq!(
                    actual.sample_rate, sample_rate,
                    "{}: sample rate",
                    case.name
                );
                assert_eq!(actual.bits_per_sample, bits, "{}: bits", case.name);
                assert_eq!(actual.frame_count, frames, "{}: frames", case.name);
            }
            (None, None) => {}
            (Some(_), None) => panic!("{}: the fast header path now declines it", case.name),
            (None, Some(actual)) => {
                panic!(
                    "{}: the fast header path now accepts it ({actual:?})",
                    case.name
                )
            }
        }
    }
}

#[test]
fn the_decoder_reads_what_the_readme_says() {
    let dir = fixture_dir();
    for case in EXPECTED {
        let path = dir.join(case.name);
        let decoded = neowaves::audio_io::decode_audio_multi(&path);
        match (case.decode, decoded) {
            (Decodes { channels, frames }, Result::Ok((actual, _))) => {
                assert_eq!(actual.len(), channels, "{}: decoded channels", case.name);
                let actual_frames = actual.first().map(|c| c.len()).unwrap_or(0);
                assert_eq!(actual_frames, frames, "{}: decoded frames", case.name);
            }
            (Fails, Err(_)) => {}
            (Decodes { .. }, Err(err)) => {
                panic!("{}: no longer decodes: {err}", case.name)
            }
            (Fails, Result::Ok((actual, _))) => panic!(
                "{}: now decodes ({} channels). If that is the improvement, update this table and the README.",
                case.name,
                actual.len()
            ),
        }
    }
}

/// A file whose markers cannot be written back in place has to be recognised
/// before anything tries.
#[test]
fn sidecar_metadata_is_required_only_where_the_readme_says() {
    let dir = fixture_dir();
    for case in EXPECTED {
        let path = dir.join(case.name);
        assert_eq!(
            neowaves::wav_stream::wave_metadata_requires_sidecar(&path),
            case.sidecar,
            "{}: sidecar requirement",
            case.name
        );
    }
}

/// `edge_chunks_after_data.wav` carries real `cue ` and `smpl` chunks, so the
/// fixture doubles as the one committed file with markers and a loop in it.
#[test]
fn chunks_behind_data_are_read_as_markers_and_a_loop() {
    let path = fixture_dir().join("edge_chunks_after_data.wav");
    let markers = neowaves::markers::read_markers(&path, 48_000, 48_000).expect("read markers");
    let positions: Vec<usize> = markers.iter().map(|m| m.sample).collect();
    assert_eq!(
        positions,
        vec![1_200, 2_400],
        "the two cue points should land a quarter and a half of the way in"
    );
    assert_eq!(
        neowaves::loop_markers::read_loop_markers(&path),
        Some((1_200, 3_600)),
        "the smpl chunk's forward loop"
    );
}

/// The point of `content_*.wav`: a mixdown divides by the channel count, so
/// twelve channels carrying one signal draw far below the signal's own level.
#[test]
fn a_twelve_channel_mixdown_buries_a_single_loud_channel() {
    let dir = fixture_dir();
    let peak = |name: &str| -> (f32, f32) {
        let (channels, _) = neowaves::audio_io::decode_audio_multi(&dir.join(name))
            .unwrap_or_else(|err| panic!("{name}: {err}"));
        let frames = channels.first().map(|c| c.len()).unwrap_or(0);
        let mut lane_peak: f32 = 0.0;
        let mut mix_peak: f32 = 0.0;
        let inv = 1.0 / channels.len().max(1) as f32;
        for i in 0..frames {
            let mut sum = 0.0f32;
            for channel in &channels {
                lane_peak = lane_peak.max(channel[i].abs());
                sum += channel[i];
            }
            mix_peak = mix_peak.max((sum * inv).abs());
        }
        (lane_peak, mix_peak)
    };

    let (lane, mix) = peak("content_ch1_only.wav");
    assert!(lane > 0.6, "the loud channel should be loud, got {lane}");
    assert!(
        mix < lane / 8.0,
        "one loud channel in twelve should average down hard: lane {lane}, mix {mix}"
    );

    // The control: the same signal on every channel averages to itself.
    let (lane, mix) = peak("content_in_phase.wav");
    assert!(
        mix > lane * 0.9,
        "identical channels should not lose level: lane {lane}, mix {mix}"
    );

    // And the opposite extreme: full-level lanes, silent mixdown.
    let (lane, mix) = peak("content_antiphase.wav");
    assert!(lane > 0.6, "antiphase lanes should be loud, got {lane}");
    assert!(
        mix < 0.01,
        "antiphase pairs should cancel in the mixdown, got {mix}"
    );
}
