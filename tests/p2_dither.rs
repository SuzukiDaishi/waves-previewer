use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use neowaves::wave::{
    codec_export_options, decode_wav_multi, export_channels_audio_with_depth,
    set_codec_export_options, TpdfDither, WavBitDepth,
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

#[test]
fn dithered_16bit_wav_roundtrip_stays_within_two_lsb() {
    let _guard = CODEC_OPTS_LOCK.lock().unwrap();
    let dir = make_temp_dir("wav16");
    let chans = synth(48_000, 0.25, 0.5);
    let mut opts = codec_export_options();
    opts.dither_16bit = true;
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
    opts.dither_16bit = false;
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
    opts.dither_16bit = true;
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
