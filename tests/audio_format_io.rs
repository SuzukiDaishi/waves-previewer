use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

fn make_temp_dir(tag: &str) -> PathBuf {
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let seq = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let mut dir = std::env::temp_dir();
    dir.push(format!(
        "neowaves_{tag}_{}_{}_{}",
        std::process::id(),
        now_ms,
        seq
    ));
    std::fs::create_dir_all(&dir).expect("create temp test dir");
    dir
}

fn synth_stereo(sr: u32, secs: f32) -> Vec<Vec<f32>> {
    let frames = ((sr as f32) * secs).max(1.0) as usize;
    let mut left = Vec::with_capacity(frames);
    let mut right = Vec::with_capacity(frames);
    for i in 0..frames {
        let t = (i as f32) / (sr as f32);
        left.push((t * 440.0 * std::f32::consts::TAU).sin() * 0.25);
        right.push((t * 660.0 * std::f32::consts::TAU).sin() * 0.20);
    }
    vec![left, right]
}

fn assert_probe_and_decode(path: &std::path::Path) {
    let info = neowaves::audio_io::read_audio_info(path)
        .unwrap_or_else(|e| panic!("probe failed for {}: {e}", path.display()));
    assert!(
        info.channels > 0,
        "channels should be > 0: {}",
        path.display()
    );
    assert!(
        info.sample_rate > 0,
        "sample_rate should be > 0: {}",
        path.display()
    );
    assert!(
        info.bits_per_sample > 0,
        "bits should be > 0: {}",
        path.display()
    );
    let (channels, sr) = neowaves::audio_io::decode_audio_multi(path)
        .unwrap_or_else(|e| panic!("decode failed for {}: {e}", path.display()));
    assert!(
        sr > 0,
        "decoded sample_rate should be > 0: {}",
        path.display()
    );
    assert!(
        !channels.is_empty(),
        "decoded channels should not be empty: {}",
        path.display()
    );
    assert!(
        channels[0].iter().all(|v| v.is_finite()),
        "decoded samples should be finite: {}",
        path.display()
    );
}

#[test]
fn audio_probe_decode_for_available_formats() {
    let dir = make_temp_dir("audio_probe_decode");
    let chans = synth_stereo(44_100, 0.20);
    // MP3 is feature-gated and AAC is intentionally unsupported, so the matrix
    // covers whatever this build can actually write.
    let formats: Vec<&str> = ["wav", "aiff", "mp3", "m4a", "ogg"]
        .into_iter()
        .filter(|ext| neowaves::wave::export_format_is_available(ext))
        .collect();
    for ext in formats {
        let path = dir.join(format!("tone.{ext}"));
        neowaves::wave::export_channels_audio(&chans, 44_100, &path)
            .unwrap_or_else(|e| panic!("export {ext} failed: {e}"));
        assert_probe_and_decode(&path);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// AAC export stays unavailable everywhere: NeoWaves has no encoder for it and
/// no video encoder to write a video back out with. Decoding follows whether
/// the operating system lends a decoder, which is what the UI labels too.
#[test]
fn aac_export_stays_unavailable_and_decode_follows_platform_support() {
    assert!(!neowaves::wave::export_format_is_available("aac"));
    assert!(!neowaves::wave::export_format_is_available("m4a"));

    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("test_samples")
        .join("video")
        .join("video_sync_6s_30fps.mp4");
    assert!(neowaves::audio_io::probe_isobmff_aac_audio_track(&fixture).expect("probe AAC fixture"));

    if neowaves::audio_io::aac_decode_available() {
        let (chans, sr) = neowaves::audio_io::decode_audio_multi(&fixture)
            .expect("the OS AAC decoder should read the fixture");
        assert!(sr >= 8_000, "implausible AAC sample rate: {sr}");
        let frames = chans.first().map(|c| c.len()).unwrap_or(0);
        assert!(
            frames as f32 / sr as f32 > 5.0,
            "expected the whole 6 s fixture, decoded {frames} frames at {sr} Hz"
        );
        assert!(
            chans.iter().any(|c| c.iter().any(|v| v.abs() > 0.01)),
            "the fixture's tones decoded to silence"
        );
    } else {
        let error = neowaves::audio_io::decode_audio_multi(&fixture)
            .expect_err("AAC must not decode without an OS decoder");
        assert!(
            error.to_string().contains("AAC decoding is not supported"),
            "unexpected AAC error: {error:#}"
        );
    }
}
