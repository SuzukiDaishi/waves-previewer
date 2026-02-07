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
fn audio_probe_decode_for_wav_mp3_m4a_ogg() {
    let dir = make_temp_dir("audio_probe_decode");
    let chans = synth_stereo(44_100, 0.20);
    let formats = ["wav", "mp3", "m4a", "ogg"];
    for ext in formats {
        let path = dir.join(format!("tone.{ext}"));
        neowaves::wave::export_channels_audio(&chans, 44_100, &path)
            .unwrap_or_else(|e| panic!("export {ext} failed: {e}"));
        assert_probe_and_decode(&path);
    }
    let _ = std::fs::remove_dir_all(&dir);
}
