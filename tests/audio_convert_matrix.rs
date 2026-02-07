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
        left.push((t * 330.0 * std::f32::consts::TAU).sin() * 0.30);
        right.push((t * 550.0 * std::f32::consts::TAU).sin() * 0.25);
    }
    vec![left, right]
}

#[test]
fn audio_convert_matrix_wav_mp3_m4a_ogg() {
    let dir = make_temp_dir("audio_convert_matrix");
    let chans = synth_stereo(44_100, 0.18);
    let formats = ["wav", "mp3", "m4a", "ogg"];

    let mut sources: Vec<(String, PathBuf)> = Vec::new();
    for ext in formats {
        let path = dir.join(format!("src_{ext}.{ext}"));
        neowaves::wave::export_channels_audio(&chans, 44_100, &path)
            .unwrap_or_else(|e| panic!("prepare source {ext} failed: {e}"));
        sources.push((ext.to_string(), path));
    }

    for (src_ext, src_path) in &sources {
        for dst_ext in formats {
            if src_ext == dst_ext {
                continue;
            }
            let dst = dir.join(format!("conv_{src_ext}_to_{dst_ext}.{dst_ext}"));
            neowaves::wave::export_gain_audio(src_path, &dst, 0.0).unwrap_or_else(|e| {
                panic!(
                    "convert {} -> {} failed ({} -> {}): {e}",
                    src_ext,
                    dst_ext,
                    src_path.display(),
                    dst.display()
                )
            });
            assert!(dst.is_file(), "missing output: {}", dst.display());
            let info = neowaves::audio_io::read_audio_info(&dst)
                .unwrap_or_else(|e| panic!("probe failed for converted {}: {e}", dst.display()));
            assert!(
                info.sample_rate > 0 && info.channels > 0,
                "invalid info for {}",
                dst.display()
            );
            let (decoded, sr) = neowaves::audio_io::decode_audio_multi(&dst)
                .unwrap_or_else(|e| panic!("decode failed for converted {}: {e}", dst.display()));
            assert!(
                sr > 0,
                "decoded sample rate should be > 0: {}",
                dst.display()
            );
            assert!(
                !decoded.is_empty() && !decoded[0].is_empty(),
                "decoded frames should not be empty: {}",
                dst.display()
            );
        }
    }

    let _ = std::fs::remove_dir_all(&dir);
}
