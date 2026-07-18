use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use neowaves::wave::{
    codec_export_options, decode_wav_multi, export_channels_audio_with_depth, f32_to_i16_sym,
    set_codec_export_options, DitherMode, Quantizer, TpdfDither, WavBitDepth,
};

// Tests below mutate the process-global codec export options; serialize them
// so parallel test threads can't interleave set/export/restore sequences.
static CODEC_OPTS_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn make_temp_dir(tag: &str) -> PathBuf {
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let seq = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "neowaves_p2_dither_{tag}_{}_{}_{}",
        std::process::id(),
        now_ms,
        seq
    ));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn synth(sr: u32, secs: f32, amp: f32) -> Vec<Vec<f32>> {
    let frames = ((sr as f32) * secs) as usize;
    let ch: Vec<f32> = (0..frames)
        .map(|i| ((i as f32 / sr as f32) * 220.0 * std::f32::consts::TAU).sin() * amp)
        .collect();
    vec![ch.clone(), ch]
}

#[test]
fn tpdf_dither_is_deterministic_and_bounded() {
    let mut a = TpdfDither::new(TpdfDither::DEFAULT_SEED);
    let mut b = TpdfDither::new(TpdfDither::DEFAULT_SEED);
    let mut sum = 0.0f64;
    const N: usize = 100_000;
    for _ in 0..N {
        let va = a.next();
        let vb = b.next();
        assert_eq!(va, vb, "same seed must reproduce the same sequence");
        assert!(va > -1.0 && va < 1.0, "TPDF sample out of range: {va}");
        sum += f64::from(va);
    }
    assert!(
        (sum / N as f64).abs() < 0.01,
        "TPDF noise should be zero-mean, got {}",
        sum / N as f64
    );
}

fn goertzel_power(x: &[f32], sr: f32, freq: f32) -> f64 {
    let w = 2.0 * std::f64::consts::PI * f64::from(freq) / f64::from(sr);
    let coeff = 2.0 * w.cos();
    let (mut s1, mut s2) = (0.0f64, 0.0f64);
    for &v in x {
        let s0 = f64::from(v) + coeff * s1 - s2;
        s2 = s1;
        s1 = s0;
    }
    (s1 * s1 + s2 * s2 - coeff * s1 * s2) / (x.len() as f64 * x.len() as f64)
}

#[test]
fn quantizer_off_matches_plain_symmetric_rounding() {
    let mut q = Quantizer::new(32768.0, i16::MIN as f64, i16::MAX as f64, 2, DitherMode::Off);
    for i in 0..20_000 {
        let v = ((i as f32) / 9_999.5 - 1.0) * 1.001; // sweep past full scale
        let expected = f32_to_i16_sym(v.clamp(-1.0, 1.0)) as i32;
        assert_eq!(q.quantize(i % 2, v.clamp(-1.0, 1.0)), expected, "v={v}");
    }
}

#[test]
fn noise_shaped_dither_moves_error_out_of_the_low_band() {
    // Quantize a quiet 220 Hz sine at 16-bit with flat TPDF vs noise-shaped
    // TPDF and compare the *error* signal's energy below 4 kHz. The 2nd-order
    // highpass NTF must push noise out of the low band.
    let sr = 48_000f32;
    let n = 32_768usize;
    let src: Vec<f32> = (0..n)
        .map(|i| ((i as f32 / sr) * 220.0 * std::f32::consts::TAU).sin() * 0.01)
        .collect();
    let low_band_error = |mode: DitherMode| -> f64 {
        let mut q = Quantizer::new(32768.0, i16::MIN as f64, i16::MAX as f64, 1, mode);
        let err: Vec<f32> = src
            .iter()
            .map(|&v| q.quantize(0, v) as f32 / 32768.0 - v)
            .collect();
        // Probe frequencies away from the 220 Hz signal residual.
        let mut total = 0.0;
        let mut f = 500.0f32;
        while f < 4_000.0 {
            total += goertzel_power(&err, sr, f);
            f += 250.0;
        }
        total
    };
    let flat = low_band_error(DitherMode::Tpdf);
    let shaped = low_band_error(DitherMode::TpdfNoiseShaped);
    assert!(
        shaped < flat * 0.5,
        "noise shaping should at least halve sub-4kHz error energy: flat={flat:e} shaped={shaped:e}"
    );
}

#[test]
fn dither_24bit_export_honors_flag() {
    let _guard = CODEC_OPTS_LOCK.lock().unwrap();
    let dir = make_temp_dir("wav24");
    let chans = synth(48_000, 0.1, 0.3);

    let export = |name: &str, mode: DitherMode, dither_24: bool, dir: &PathBuf| -> Vec<u8> {
        let mut opts = codec_export_options();
        opts.dither_mode = mode;
        opts.dither_24bit = dither_24;
        set_codec_export_options(opts);
        let dst = dir.join(name);
        export_channels_audio_with_depth(&chans, 48_000, &dst, Some(WavBitDepth::Pcm24))
            .expect("export 24-bit wav");
        std::fs::read(&dst).expect("read exported wav")
    };
    // Flag off: 24-bit output is identical to plain rounding even with a
    // dither mode selected (16-bit-only by default).
    let plain = export("plain.wav", DitherMode::Off, false, &dir);
    let flag_off = export("flag_off.wav", DitherMode::Tpdf, false, &dir);
    assert_eq!(plain, flag_off, "24-bit must not dither with the flag off");
    // Flag on: dither reaches the 24-bit quantizer.
    let flag_on = export("flag_on.wav", DitherMode::Tpdf, true, &dir);
    assert_ne!(plain, flag_on, "24-bit should dither with the flag on");

    set_codec_export_options(Default::default());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn noise_shaped_flac_two_pass_md5_stays_consistent() {
    let _guard = CODEC_OPTS_LOCK.lock().unwrap();
    let dir = make_temp_dir("flac_ns");
    let chans = synth(44_100, 0.25, 0.4);
    let mut opts = codec_export_options();
    opts.dither_mode = DitherMode::TpdfNoiseShaped;
    set_codec_export_options(opts);

    let dst = dir.join("shaped.flac");
    export_channels_audio_with_depth(&chans, 44_100, &dst, Some(WavBitDepth::Pcm16))
        .expect("export noise-shaped 16-bit flac");
    // A sample-accurate decode proves the MD5 pass and the encode pass
    // replayed the identical dither + error-feedback sequence.
    let decoded = neowaves::audio_io::decode_audio_multi(&dst).expect("decode flac");
    assert_eq!(decoded.1, 44_100);
    let n = chans[0].len().min(decoded.0[0].len());
    let lsb = 1.0 / 32768.0f32;
    let mut max_err = 0.0f32;
    for k in 0..n {
        max_err = max_err.max((decoded.0[0][k] - chans[0][k]).abs());
    }
    // Shaped noise has a higher peak than flat TPDF; allow a wider (but
    // still tiny) bound.
    assert!(max_err < 8.0 * lsb, "flac roundtrip error too large: {max_err}");

    set_codec_export_options(Default::default());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn dithered_16bit_wav_roundtrip_stays_within_two_lsb() {
    let _guard = CODEC_OPTS_LOCK.lock().unwrap();
    let dir = make_temp_dir("wav16");
    let chans = synth(48_000, 0.25, 0.5);
    let mut opts = codec_export_options();
    opts.dither_mode = DitherMode::Tpdf;
    set_codec_export_options(opts);

    let dst = dir.join("dithered.wav");
    export_channels_audio_with_depth(&chans, 48_000, &dst, Some(WavBitDepth::Pcm16))
        .expect("export 16-bit wav");
    let (decoded, sr) = decode_wav_multi(&dst).expect("decode");
    assert_eq!(sr, 48_000);
    assert_eq!(decoded.len(), chans.len());

    let lsb = 1.0 / 32768.0f32;
    let mut max_err = 0.0f32;
    let mut sum_sq = 0.0f64;
    let n = chans[0].len().min(decoded[0].len());
    assert!(n > 0);
    for k in 0..n {
        let err = (decoded[0][k] - chans[0][k]).abs();
        max_err = max_err.max(err);
        sum_sq += f64::from(err) * f64::from(err);
    }
    // TPDF adds at most +/-1 LSB before rounding: total error stays < 2 LSB.
    assert!(
        max_err < 2.0 * lsb,
        "max error {max_err} should stay below 2 LSB ({})",
        2.0 * lsb
    );
    let rms = (sum_sq / n as f64).sqrt() as f32;
    assert!(rms < lsb, "rms error {rms} should stay below 1 LSB");

    // Dither actually changes the quantization result vs. plain rounding.
    let mut opts = codec_export_options();
    opts.dither_mode = DitherMode::Off;
    set_codec_export_options(opts);
    let dst_plain = dir.join("plain.wav");
    export_channels_audio_with_depth(&chans, 48_000, &dst_plain, Some(WavBitDepth::Pcm16))
        .expect("export plain 16-bit wav");
    let (plain, _) = decode_wav_multi(&dst_plain).expect("decode plain");
    let differing = (0..n).filter(|&k| plain[0][k] != decoded[0][k]).count();
    assert!(
        differing > n / 100,
        "dithered output should differ from plain rounding ({differing}/{n} samples differ)"
    );

    // Restore the default so other tests in this binary see it.
    set_codec_export_options(Default::default());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn dithered_flac_16bit_roundtrip_decodes_consistently() {
    let _guard = CODEC_OPTS_LOCK.lock().unwrap();
    let dir = make_temp_dir("flac16");
    let chans = synth(44_100, 0.25, 0.4);
    let mut opts = codec_export_options();
    opts.dither_mode = DitherMode::Tpdf;
    set_codec_export_options(opts);

    let dst = dir.join("dithered.flac");
    export_channels_audio_with_depth(&chans, 44_100, &dst, Some(WavBitDepth::Pcm16))
        .expect("export 16-bit flac");
    // Decode through the app's generic decoder; the STREAMINFO MD5 was
    // computed in a separate pass from the encode pass, so a successful,
    // sample-accurate decode proves both passes dithered identically.
    let decoded = neowaves::audio_io::decode_audio_multi(&dst).expect("decode flac");
    assert_eq!(decoded.1, 44_100);
    assert_eq!(decoded.0.len(), chans.len());
    let n = chans[0].len().min(decoded.0[0].len());
    assert!(n > 0);
    let lsb = 1.0 / 32768.0f32;
    let mut max_err = 0.0f32;
    for k in 0..n {
        max_err = max_err.max((decoded.0[0][k] - chans[0][k]).abs());
    }
    assert!(max_err < 2.5 * lsb, "flac roundtrip error too large: {max_err}");

    set_codec_export_options(Default::default());
    let _ = std::fs::remove_dir_all(&dir);
}
