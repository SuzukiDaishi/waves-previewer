//! Writes the WAV fixture matrix that lives in `test_samples/formats`.
//!
//! The committed corpus before this tool was 16-bit mono and 24-bit stereo at
//! 44.1 kHz, plus MP3 and MP4. Nothing multichannel, nothing at any other rate,
//! no 8-bit, no float, no `WAVE_FORMAT_EXTENSIBLE`, no RF64. A 12-channel file
//! that opened the editor on a black canvas went unnoticed for exactly that
//! reason: there was no file of that shape to open.
//!
//! Every file here is a deterministic function of its name. Regenerating must
//! leave `git status` clean, so nothing may depend on the clock, on entropy, or
//! on iteration order.
//!
//! ```text
//! cargo run --manifest-path tools/gen-wav-fixtures/Cargo.toml -- --list
//! cargo run --manifest-path tools/gen-wav-fixtures/Cargo.toml
//! cargo run --manifest-path tools/gen-wav-fixtures/Cargo.toml -- --huge
//! ```

mod signal;
mod wav;

use std::path::{Path, PathBuf};

use wav::{Container, Encoding, Extensible, WavSpec};

/// One file: a name and the bytes it should hold.
struct Fixture {
    name: String,
    bytes: Vec<u8>,
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut out: Option<PathBuf> = None;
    let mut only: Option<String> = None;
    let mut list = false;
    let mut huge = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--out" => {
                i += 1;
                match args.get(i) {
                    Some(value) => out = Some(PathBuf::from(value)),
                    None => fail("--out needs a directory"),
                }
            }
            "--only" => {
                i += 1;
                match args.get(i) {
                    Some(value) => only = Some(value.clone()),
                    None => fail("--only needs a name prefix"),
                }
            }
            "--list" => list = true,
            "--huge" => huge = true,
            "-h" | "--help" => {
                print_help();
                return;
            }
            other => fail(&format!("unknown argument: {other}")),
        }
        i += 1;
    }

    let out_dir = out.unwrap_or_else(|| repo_root().join("test_samples/formats"));
    let mut fixtures = build_fixtures();
    if let Some(prefix) = &only {
        fixtures.retain(|f| f.name.starts_with(prefix.as_str()));
        if fixtures.is_empty() {
            fail(&format!("no fixture name starts with {prefix:?}"));
        }
    }

    if list {
        let mut total = 0usize;
        for fixture in &fixtures {
            println!("{:>10}  {}", fixture.bytes.len(), fixture.name);
            total += fixture.bytes.len();
        }
        println!(
            "{:>10}  ({} files, {:.2} MiB)",
            total,
            fixtures.len(),
            total as f64 / (1024.0 * 1024.0)
        );
        if huge {
            println!("\n--huge would additionally write {}", huge_description());
        }
        return;
    }

    if let Err(err) = std::fs::create_dir_all(&out_dir) {
        fail(&format!("create {}: {err}", out_dir.display()));
    }
    let mut written = 0usize;
    for fixture in &fixtures {
        let path = out_dir.join(&fixture.name);
        if let Err(err) = std::fs::write(&path, &fixture.bytes) {
            fail(&format!("write {}: {err}", path.display()));
        }
        written += fixture.bytes.len();
    }
    println!(
        "wrote {} files ({:.2} MiB) to {}",
        fixtures.len(),
        written as f64 / (1024.0 * 1024.0),
        out_dir.display()
    );

    if huge {
        write_huge();
    }
}

fn print_help() {
    println!(
        "gen-wav-fixtures -- write the WAV fixture matrix\n\
         \n\
         --out DIR      where to write (default: test_samples/formats)\n\
         --only PREFIX  only fixtures whose name starts with PREFIX\n\
         --list         print names and sizes without writing anything\n\
         --huge         also write the oversized fixture into debug/ (gitignored)\n"
    );
}

fn fail(message: &str) -> ! {
    eprintln!("gen-wav-fixtures: {message}");
    std::process::exit(2);
}

/// The repository root, resolved from this crate rather than the shell's
/// working directory, so the tool behaves the same wherever it is run from.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

const SR: u32 = 48_000;

fn frames(sample_rate: u32, secs: f32) -> usize {
    (sample_rate as f32 * secs).round() as usize
}

fn push(fixtures: &mut Vec<Fixture>, name: &str, bytes: Vec<u8>) {
    fixtures.push(Fixture {
        name: name.to_string(),
        bytes,
    });
}

fn build_fixtures() -> Vec<Fixture> {
    let mut f = Vec::new();
    channel_sweep(&mut f);
    content_patterns(&mut f);
    bit_depths(&mut f);
    sample_rates(&mut f);
    header_edge_cases(&mut f);
    f
}

// ---------------------------------------------------------------------------
// A. Channel counts
// ---------------------------------------------------------------------------

/// Longer where the file is meant to be looked at, shorter where the only
/// question is whether it opens at all.
fn channel_sweep(f: &mut Vec<Fixture>) {
    // Layouts with a name: 6 is 5.1, 8 is 7.1, 12 is 7.1.4. Long enough and
    // deep enough to actually read the lanes.
    for ch in [1usize, 2, 6, 8, 12] {
        let n = frames(SR, 1.0);
        let spec = spec_for(ch as u16, SR, Encoding::PcmI24);
        push(
            f,
            &format!("ch_{ch:02}.wav"),
            spec.render(&signal::lanes(ch, SR, n)),
        );
    }
    // The rest of what `audio_channels::standard_layout` knows, plus 9 and 11,
    // which it does not: those fall back to index arithmetic.
    for ch in [3usize, 4, 9, 10, 11, 16] {
        let n = frames(SR, 0.25);
        let spec = spec_for(ch as u16, SR, Encoding::PcmI16);
        push(
            f,
            &format!("ch_{ch:02}.wav"),
            spec.render(&signal::lanes(ch, SR, n)),
        );
    }
    // Symphonia's `Channels` bitmask defines 26 bits. Past that the extensible
    // header cannot name the channels and the file will not decode, so 26 and
    // 27 sit either side of a real cliff.
    for ch in [24usize, 26, 27, 32] {
        let n = frames(SR, 0.1);
        let spec = spec_for(ch as u16, SR, Encoding::PcmI16);
        push(
            f,
            &format!("ch_{ch:02}.wav"),
            spec.render(&signal::lanes(ch, SR, n)),
        );
    }
}

/// Past two channels or 16 bits, every real writer uses the extensible header;
/// match that so the sweep is not also an accidental header test.
fn spec_for(channels: u16, sample_rate: u32, encoding: Encoding) -> WavSpec {
    let spec = WavSpec::new(channels, sample_rate, encoding);
    if channels > 2 || encoding.bits() > 16 {
        spec.extensible()
    } else {
        spec
    }
}

// ---------------------------------------------------------------------------
// B. What the samples contain
// ---------------------------------------------------------------------------

/// Twelve channels each, differing only in what is in them.
///
/// Every mixdown in the editor averages over the channel count, so these are
/// the files that show what that costs.
fn content_patterns(f: &mut Vec<Fixture>) {
    let ch = 12usize;
    let n = frames(SR, 1.0);
    let spec = || spec_for(ch as u16, SR, Encoding::PcmI16);

    // Loud on channel 1, silent everywhere else. A mixdown divides by 12, so
    // this draws 21.6 dB down -- flat enough to read as "nothing loaded".
    let mut ch1_only = vec![signal::silence(n); ch];
    ch1_only[0] = signal::tone(220.0, 0.71, SR, n);
    push(f, "content_ch1_only.wav", spec().render(&ch1_only));

    // The same tone on every channel: the mixdown is the tone, undiminished.
    let in_phase = vec![signal::tone(220.0, 0.71, SR, n); ch];
    push(f, "content_in_phase.wav", spec().render(&in_phase));

    // Six pairs, each the negative of the other. The mixdown cancels to
    // silence while every lane is at full level.
    let base = signal::tone(220.0, 0.71, SR, n);
    let inverted: Vec<f32> = base.iter().map(|v| -v).collect();
    let antiphase: Vec<Vec<f32>> = (0..ch)
        .map(|c| {
            if c % 2 == 0 {
                base.clone()
            } else {
                inverted.clone()
            }
        })
        .collect();
    push(f, "content_antiphase.wav", spec().render(&antiphase));

    // Each channel its own pitch and its own burst, so a lane can be traced
    // back to a channel by eye.
    push(
        f,
        "content_per_channel_tones.wav",
        spec().render(&signal::lanes(ch, SR, n)),
    );
}

// ---------------------------------------------------------------------------
// C. Bit depths
// ---------------------------------------------------------------------------

/// The whole of `read_wave_pcm_info`'s accepted table:
/// `(1,8) | (1,16) | (1,24) | (1,32) | (3,32)`.
fn bit_depths(f: &mut Vec<Fixture>) {
    let n = frames(SR, 0.5);
    let stereo = |amp: f32| {
        vec![
            signal::tone(220.0, amp, SR, n),
            signal::tone(330.0, amp, SR, n),
        ]
    };
    for (name, encoding) in [
        ("depth_08bit.wav", Encoding::PcmU8),
        ("depth_16bit.wav", Encoding::PcmI16),
        ("depth_24bit.wav", Encoding::PcmI24),
        ("depth_32int.wav", Encoding::PcmI32),
        ("depth_32float.wav", Encoding::F32),
    ] {
        let spec = spec_for(2, SR, encoding);
        push(f, name, spec.render(&stereo(0.71)));
    }
}

// ---------------------------------------------------------------------------
// D. Sample rates
// ---------------------------------------------------------------------------

fn sample_rates(f: &mut Vec<Fixture>) {
    for sr in [
        8_000u32, 11_025, 16_000, 22_050, 32_000, 44_100, 48_000, 88_200, 96_000, 176_400, 192_000,
    ] {
        let n = frames(sr, 0.5);
        let spec = spec_for(2, sr, Encoding::PcmI16);
        let chans = vec![
            signal::tone(220.0, 0.71, sr, n),
            signal::tone(330.0, 0.71, sr, n),
        ];
        push(f, &format!("rate_{sr:06}.wav"), spec.render(&chans));
    }
}

// ---------------------------------------------------------------------------
// E. Headers no ordinary writer produces
// ---------------------------------------------------------------------------

fn header_edge_cases(f: &mut Vec<Fixture>) {
    let ch = 12usize;
    let n = frames(SR, 0.1);
    let twelve = || signal::lanes(ch, SR, n);
    let stereo_n = frames(SR, 0.1);
    let stereo = || {
        vec![
            signal::tone(220.0, 0.71, SR, stereo_n),
            signal::tone(330.0, 0.71, SR, stereo_n),
        ]
    };

    // The mask a 7.1.4 bed actually carries: FL FR FC LFE BL BR SL SR TFL TFR
    // TBL TBR. Twelve bits set, but not the lowest twelve.
    const MASK_7_1_4: u32 = 0x0002_D63F;
    push(
        f,
        "edge_ext_mask_714.wav",
        WavSpec::new(ch as u16, SR, Encoding::PcmI16)
            .with_extensible(Extensible {
                valid_bits: 16,
                channel_mask: MASK_7_1_4,
                subformat: wav::SUBFORMAT_PCM,
            })
            .render(&twelve()),
    );

    // What ffmpeg writes when the layout has no name: no mask at all.
    push(
        f,
        "edge_ext_mask_zero.wav",
        WavSpec::new(ch as u16, SR, Encoding::PcmI16)
            .with_extensible(Extensible {
                valid_bits: 16,
                channel_mask: 0,
                subformat: wav::SUBFORMAT_PCM,
            })
            .render(&twelve()),
    );

    // Fewer mask bits than channels, and more: both are repaired by filling in
    // or dropping the high bits.
    push(
        f,
        "edge_ext_mask_too_few.wav",
        WavSpec::new(ch as u16, SR, Encoding::PcmI16)
            .with_extensible(Extensible {
                valid_bits: 16,
                channel_mask: 0b1111,
                subformat: wav::SUBFORMAT_PCM,
            })
            .render(&twelve()),
    );
    push(
        f,
        "edge_ext_mask_too_many.wav",
        WavSpec::new(ch as u16, SR, Encoding::PcmI16)
            .with_extensible(Extensible {
                valid_bits: 16,
                channel_mask: 0x0003_FFFF,
                subformat: wav::SUBFORMAT_PCM,
            })
            .render(&twelve()),
    );

    // An extensible header whose sub-format is neither PCM nor IEEE float.
    push(
        f,
        "edge_ext_subformat_unknown.wav",
        WavSpec::new(2, SR, Encoding::PcmI16)
            .with_extensible(Extensible {
                valid_bits: 16,
                channel_mask: 0b11,
                subformat: wav::SUBFORMAT_UNKNOWN,
            })
            .render(&stereo()),
    );

    // 64-bit float: outside the fast header path's table, inside what the
    // decoder can actually read.
    push(
        f,
        "edge_float64.wav",
        WavSpec::new(2, SR, Encoding::F64).render(&stereo()),
    );

    // An odd number of data bytes, which forces the pad byte before the next
    // chunk. 8-bit mono at an odd frame count is the simplest way to get one.
    let odd_frames = frames(SR, 0.1) | 1;
    let odd = vec![signal::tone(220.0, 0.71, SR, odd_frames)];
    let mut odd_spec = WavSpec::new(1, SR, Encoding::PcmU8);
    odd_spec
        .after_data
        .push(wav::list_info_chunk("odd data chunk"));
    push(f, "edge_data_odd_len.wav", odd_spec.render(&odd));

    // Chunks ahead of `fmt `, which a parser that assumes a fixed offset will
    // read straight past.
    let mut before = WavSpec::new(2, SR, Encoding::PcmI16);
    before.before_fmt.push(wav::junk_chunk(28));
    before.before_fmt.push(wav::bext_chunk("NeoWaves fixture"));
    before
        .before_fmt
        .push(wav::list_info_chunk("chunks before fmt"));
    push(f, "edge_chunks_before_fmt.wav", before.render(&stereo()));

    // Chunks behind `data`, where the markers and loop points live.
    let mut after = WavSpec::new(2, SR, Encoding::PcmI16);
    after.after_data.push(wav::cue_chunk(&[
        (stereo_n / 4) as u32,
        (stereo_n / 2) as u32,
    ]));
    after.after_data.push(wav::smpl_chunk(
        SR,
        (stereo_n / 4) as u32,
        (stereo_n * 3 / 4) as u32,
    ));
    after
        .after_data
        .push(wav::list_info_chunk("chunks after data"));
    push(f, "edge_chunks_after_data.wav", after.render(&stereo()));

    // `data` claims twice the bytes the file holds.
    let real_len = (stereo_n * 2 * 2) as u32;
    let mut truncated = WavSpec::new(2, SR, Encoding::PcmI16);
    truncated.data_len_override = Some(real_len * 2);
    push(f, "edge_data_truncated.wav", truncated.render(&stereo()));

    // `data` with nothing in it.
    let mut empty = WavSpec::new(2, SR, Encoding::PcmI16);
    empty.data_len_override = Some(0);
    push(f, "edge_data_zero.wav", empty.wrap(Vec::new()));

    // A block alignment that is not a whole number of frames.
    let mut misaligned = WavSpec::new(3, SR, Encoding::PcmI16);
    misaligned.block_align_override = Some(7);
    let three = signal::lanes(3, SR, stereo_n);
    push(
        f,
        "edge_block_align_mismatch.wav",
        misaligned.render(&three),
    );

    // RF64 and BW64: the app's own header reader takes them, the decoder wants
    // a literal `RIFF`.
    push(
        f,
        "edge_rf64.wav",
        WavSpec::new(2, SR, Encoding::PcmI16)
            .container(Container::Rf64)
            .render(&stereo()),
    );
    push(
        f,
        "edge_bw64.wav",
        WavSpec::new(2, SR, Encoding::PcmI16)
            .container(Container::Bw64)
            .render(&stereo()),
    );

    // Not a WAV at all, named like one.
    push(
        f,
        "edge_not_a_wav.wav",
        b"this is not a wav file at all\n".to_vec(),
    );
}

// ---------------------------------------------------------------------------
// The oversized one, which is never committed
// ---------------------------------------------------------------------------

/// Frames needed for 12 channels to decode past the 256 MiB resident ceiling.
const HUGE_CHANNELS: usize = 12;
const HUGE_FRAMES: usize = 268_435_456 / (HUGE_CHANNELS * 4) + 1;

fn huge_description() -> String {
    format!(
        "debug/huge_{HUGE_CHANNELS}ch_8bit_48k.wav ({:.1} s, {:.0} MiB on disk, {:.0} MiB decoded)",
        HUGE_FRAMES as f64 / SR as f64,
        (HUGE_FRAMES * HUGE_CHANNELS) as f64 / (1024.0 * 1024.0),
        (HUGE_FRAMES * HUGE_CHANNELS * 4) as f64 / (1024.0 * 1024.0),
    )
}

/// Write the one fixture that trips the paged editor for real.
///
/// 8-bit keeps it as small as it can be: the ceiling is on the *decoded* size,
/// `frames x channels x 4`, so the shallowest depth still crosses it at a
/// quarter of the bytes on disk. That is 64 MiB, which is why this is not
/// committed -- `debug/` is gitignored.
///
/// The alternative, and the cheaper one, is to leave the file alone and lower
/// the ceiling instead: `NEOWAVES_MAX_RESIDENT_DECODE_BYTES=200000` makes any
/// of the small 12-channel fixtures take the same path.
fn write_huge() {
    let dir = repo_root().join("debug");
    if let Err(err) = std::fs::create_dir_all(&dir) {
        fail(&format!("create {}: {err}", dir.display()));
    }
    let path = dir.join(format!("huge_{HUGE_CHANNELS}ch_8bit_48k.wav"));

    // Encoded a chunk of frames at a time: the whole file as f32 planes would
    // be 12 GiB of scratch for a 64 MiB result.
    const CHUNK_FRAMES: usize = 48_000;
    let mut data = Vec::with_capacity(HUGE_FRAMES * HUGE_CHANNELS);
    let mut written = 0usize;
    while written < HUGE_FRAMES {
        let take = CHUNK_FRAMES.min(HUGE_FRAMES - written);
        for i in 0..take {
            let frame = written + i;
            let t_norm = frame as f32 / (HUGE_FRAMES - 1) as f32;
            for ch in 0..HUGE_CHANNELS {
                let t = frame as f32 / SR as f32;
                let freq = signal::channel_freq(ch);
                // A slow sweep across the channels so the position in the file
                // is readable from the picture.
                let center = (ch as f32 + 0.5) / HUGE_CHANNELS as f32;
                let amp = 0.18 + 0.75 * (1.0 - ((t_norm - center).abs() * 6.0).min(1.0));
                Encoding::PcmU8.encode((t * freq * std::f32::consts::TAU).sin() * amp, &mut data);
            }
        }
        written += take;
    }

    let spec = spec_for(HUGE_CHANNELS as u16, SR, Encoding::PcmU8);
    let bytes = spec.wrap(data);
    if let Err(err) = std::fs::write(&path, &bytes) {
        fail(&format!("write {}: {err}", path.display()));
    }
    println!(
        "wrote {} ({:.1} MiB on disk, {:.0} MiB decoded)",
        path.display(),
        bytes.len() as f64 / (1024.0 * 1024.0),
        (HUGE_FRAMES * HUGE_CHANNELS * 4) as f64 / (1024.0 * 1024.0),
    );
}
