use std::cell::Cell;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

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
        left.push((t * 220.0 * std::f32::consts::TAU).sin() * 0.30);
        right.push((t * 440.0 * std::f32::consts::TAU).sin() * 0.25);
    }
    vec![left, right]
}

fn make_mp3_fixture(tag: &str, secs: f32) -> PathBuf {
    let dir = make_temp_dir(tag);
    let sr = 44_100;
    let chans = synth_stereo(sr, secs);
    let path = dir.join("fixture.mp3");
    neowaves::wave::export_channels_audio(&chans, sr, &path)
        .unwrap_or_else(|e| panic!("failed to build mp3 fixture {}: {e}", path.display()));
    path
}

struct QualityProfile {
    first_ms: f64,
    final_ms: f64,
    emits: usize,
    underruns: usize,
    worst_gap_ms: f64,
    final_frames: usize,
}

fn run_profile_with_linear(path: &std::path::Path) -> QualityProfile {
    let out_sr = 48_000u32;
    let start = Instant::now();
    let mut first_ms = 0.0f64;
    let mut final_ms = 0.0f64;
    let mut emits = 0usize;
    let mut underruns = 0usize;
    let mut worst_gap_ms = 0.0f64;
    let mut final_frames = 0usize;
    let mut play_start: Option<Instant> = None;
    let mut prev_len = 0usize;

    neowaves::audio_io::decode_audio_multi_progressive(
        path,
        1.2,
        0.75,
        || false,
        |mut channels, in_sr, is_final| {
            if in_sr != out_sr {
                for ch in channels.iter_mut() {
                    *ch = neowaves::wave::resample_linear(ch, in_sr, out_sr);
                }
            }
            let len = channels.get(0).map(|c| c.len()).unwrap_or(0);
            assert!(
                len >= prev_len,
                "progressive buffer length regressed: prev={} now={}",
                prev_len,
                len
            );
            prev_len = len;
            emits += 1;
            let elapsed = start.elapsed();
            if play_start.is_none() {
                play_start = Some(Instant::now());
                first_ms = elapsed.as_secs_f64() * 1000.0;
            }
            if let Some(ps) = play_start {
                let consumed = ps.elapsed().as_secs_f64() * out_sr as f64;
                if consumed > len as f64 {
                    underruns = underruns.saturating_add(1);
                    let gap_ms = ((consumed - len as f64) / out_sr as f64) * 1000.0;
                    if gap_ms > worst_gap_ms {
                        worst_gap_ms = gap_ms;
                    }
                }
            }
            if is_final {
                final_ms = elapsed.as_secs_f64() * 1000.0;
                final_frames = len;
            }
            true
        },
    )
    .unwrap_or_else(|e| panic!("linear profile decode failed for {}: {e}", path.display()));

    QualityProfile {
        first_ms,
        final_ms,
        emits,
        underruns,
        worst_gap_ms,
        final_frames,
    }
}

fn run_profile_with_quality(
    path: &std::path::Path,
    quality: neowaves::wave::ResampleQuality,
) -> QualityProfile {
    let out_sr = 48_000u32;
    let start = Instant::now();
    let mut first_ms = 0.0f64;
    let mut final_ms = 0.0f64;
    let mut emits = 0usize;
    let mut underruns = 0usize;
    let mut worst_gap_ms = 0.0f64;
    let mut final_frames = 0usize;
    let mut play_start: Option<Instant> = None;
    let mut prev_len = 0usize;

    neowaves::audio_io::decode_audio_multi_progressive(
        path,
        1.2,
        0.75,
        || false,
        |mut channels, in_sr, is_final| {
            if in_sr != out_sr {
                for ch in channels.iter_mut() {
                    *ch = neowaves::wave::resample_quality(ch, in_sr, out_sr, quality);
                }
            }
            let len = channels.get(0).map(|c| c.len()).unwrap_or(0);
            assert!(
                len >= prev_len,
                "progressive buffer length regressed: prev={} now={}",
                prev_len,
                len
            );
            prev_len = len;
            emits += 1;
            let elapsed = start.elapsed();
            if play_start.is_none() {
                play_start = Some(Instant::now());
                first_ms = elapsed.as_secs_f64() * 1000.0;
            }
            if let Some(ps) = play_start {
                let consumed = ps.elapsed().as_secs_f64() * out_sr as f64;
                if consumed > len as f64 {
                    underruns = underruns.saturating_add(1);
                    let gap_ms = ((consumed - len as f64) / out_sr as f64) * 1000.0;
                    if gap_ms > worst_gap_ms {
                        worst_gap_ms = gap_ms;
                    }
                }
            }
            if is_final {
                final_ms = elapsed.as_secs_f64() * 1000.0;
                final_frames = len;
            }
            true
        },
    )
    .unwrap_or_else(|e| panic!("profile decode failed for {}: {e}", path.display()));

    QualityProfile {
        first_ms,
        final_ms,
        emits,
        underruns,
        worst_gap_ms,
        final_frames,
    }
}

#[test]
fn mp3_progressive_decode_timing_and_continuity() {
    let path = make_mp3_fixture("mp3_progressive_decode_timing", 20.0);
    let dir = path.parent().expect("temp dir").to_path_buf();

    let full_start = Instant::now();
    let (full_channels, full_sr) = neowaves::audio_io::decode_audio_multi(&path)
        .unwrap_or_else(|e| panic!("full decode failed for {}: {e}", path.display()));
    let full_ms = full_start.elapsed().as_secs_f64() * 1000.0;
    assert!(!full_channels.is_empty() && !full_channels[0].is_empty());

    let prefix_secs = 1.2f32;
    let progressive_start = Instant::now();
    let mut prefix_channels: Option<Vec<Vec<f32>>> = None;
    let mut final_channels: Option<Vec<Vec<f32>>> = None;
    let mut prefix_ms = 0.0f64;
    let mut final_ms = 0.0f64;

    neowaves::audio_io::decode_audio_multi_progressive(
        &path,
        prefix_secs,
        0.75,
        || false,
        |channels, sr, is_final| {
            assert_eq!(
                sr, full_sr,
                "sample rate should stay stable for progressive decode"
            );
            if is_final {
                final_ms = progressive_start.elapsed().as_secs_f64() * 1000.0;
                final_channels = Some(channels);
            } else if prefix_channels.is_none() {
                prefix_ms = progressive_start.elapsed().as_secs_f64() * 1000.0;
                prefix_channels = Some(channels);
            }
            true
        },
    )
    .unwrap_or_else(|e| panic!("progressive decode failed for {}: {e}", path.display()));

    let prefix = prefix_channels.expect("progressive decode must emit prefix chunk");
    let final_out = final_channels.expect("progressive decode must emit final chunk");
    assert_eq!(prefix.len(), final_out.len());
    assert!(prefix[0].len() > 0, "prefix should contain samples");
    assert!(
        final_out[0].len() >= prefix[0].len(),
        "final decode must be at least as long as prefix"
    );

    for (p, f) in prefix.iter().zip(final_out.iter()) {
        let n = p.len().min(f.len());
        for i in 0..n {
            let d = (p[i] - f[i]).abs();
            assert!(
                d <= 1e-6,
                "prefix/final mismatch at sample {i}: diff={d} (possible audible handoff glitch)"
            );
        }
    }

    assert!(
        prefix_ms <= final_ms,
        "prefix latency should be <= final latency: prefix={prefix_ms:.2}ms final={final_ms:.2}ms"
    );
    assert!(
        prefix_ms < full_ms,
        "prefix latency should be faster than full decode baseline: prefix={prefix_ms:.2}ms full={full_ms:.2}ms"
    );

    eprintln!(
        "mp3_progressive_decode_timing: prefix_ms={prefix_ms:.2} final_ms={final_ms:.2} full_ms={full_ms:.2} prefix_frames={} final_frames={}",
        prefix[0].len(),
        final_out[0].len()
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn mp3_progressive_quality_profile() {
    let path = make_mp3_fixture("mp3_progressive_quality_profile", 6.0);
    let dir = path.parent().expect("temp dir").to_path_buf();

    let fast = run_profile_with_quality(&path, neowaves::wave::ResampleQuality::Fast);
    let good = run_profile_with_quality(&path, neowaves::wave::ResampleQuality::Good);
    let best = run_profile_with_quality(&path, neowaves::wave::ResampleQuality::Best);

    assert!(
        fast.first_ms > 0.0 && good.first_ms > 0.0 && best.first_ms > 0.0,
        "all profiles should report a first chunk"
    );
    assert!(
        fast.final_frames > 0 && good.final_frames > 0 && best.final_frames > 0,
        "final frame count must be > 0 for all qualities"
    );
    assert!(
        fast.final_ms <= good.final_ms + 2000.0,
        "fast should not be significantly slower than good (fast={:.2}ms good={:.2}ms)",
        fast.final_ms,
        good.final_ms
    );

    eprintln!(
        "mp3_quality_profile fast: first={:.2}ms final={:.2}ms emits={} underruns={} worst_gap={:.2}ms frames={}",
        fast.first_ms, fast.final_ms, fast.emits, fast.underruns, fast.worst_gap_ms, fast.final_frames
    );
    eprintln!(
        "mp3_quality_profile good: first={:.2}ms final={:.2}ms emits={} underruns={} worst_gap={:.2}ms frames={}",
        good.first_ms, good.final_ms, good.emits, good.underruns, good.worst_gap_ms, good.final_frames
    );
    eprintln!(
        "mp3_quality_profile best: first={:.2}ms final={:.2}ms emits={} underruns={} worst_gap={:.2}ms frames={}",
        best.first_ms, best.final_ms, best.emits, best.underruns, best.worst_gap_ms, best.final_frames
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn mp3_list_linear_profile_18s_no_underrun() {
    let path = make_mp3_fixture("mp3_list_linear_profile_18s", 18.0);
    let dir = path.parent().expect("temp dir").to_path_buf();
    let p = run_profile_with_linear(&path);
    assert!(
        p.first_ms < 1200.0,
        "list linear profile first chunk too late: {:.2}ms",
        p.first_ms
    );
    assert_eq!(
        p.underruns, 0,
        "list linear profile should avoid underrun: count={} worst_gap_ms={:.2}",
        p.underruns, p.worst_gap_ms
    );
    eprintln!(
        "mp3_list_linear_profile: first={:.2}ms final={:.2}ms emits={} underruns={} worst_gap={:.2}ms frames={}",
        p.first_ms, p.final_ms, p.emits, p.underruns, p.worst_gap_ms, p.final_frames
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn mp3_progressive_decode_cancel_stops_stale_final_chunk() {
    let path = make_mp3_fixture("mp3_progressive_decode_cancel", 12.0);
    let dir = path.parent().expect("temp dir").to_path_buf();

    let mut callback_count = 0usize;
    let mut got_final = false;
    let cancel = Cell::new(false);
    neowaves::audio_io::decode_audio_multi_progressive(
        &path,
        1.0,
        0.75,
        || cancel.get(),
        |_channels, _sr, is_final| {
            callback_count += 1;
            if is_final {
                got_final = true;
            } else {
                cancel.set(true);
            }
            true
        },
    )
    .unwrap_or_else(|e| {
        panic!(
            "progressive cancel decode failed for {}: {e}",
            path.display()
        )
    });

    assert_eq!(
        callback_count, 1,
        "cancel-after-prefix should produce exactly one callback (prefix)"
    );
    assert!(
        !got_final,
        "stale final chunk should not be emitted after cancel"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn mp3_progressive_handoff_keeps_playhead_position() {
    let path = make_mp3_fixture("mp3_progressive_handoff", 8.0);
    let dir = path.parent().expect("temp dir").to_path_buf();

    let mut prefix_channels: Option<Vec<Vec<f32>>> = None;
    let mut final_channels: Option<Vec<Vec<f32>>> = None;
    neowaves::audio_io::decode_audio_multi_progressive(
        &path,
        1.0,
        0.75,
        || false,
        |channels, _sr, is_final| {
            if is_final {
                final_channels = Some(channels);
            } else if prefix_channels.is_none() {
                prefix_channels = Some(channels);
            }
            true
        },
    )
    .unwrap_or_else(|e| panic!("progressive decode failed for {}: {e}", path.display()));

    let prefix = prefix_channels.expect("prefix chunk required");
    let full = final_channels.expect("final chunk required");
    assert!(
        prefix[0].len() > 100,
        "prefix should be long enough for seek test"
    );
    let seek_sample = (prefix[0].len() * 8) / 10;

    let engine = neowaves::audio::AudioEngine::new_for_test();
    engine.set_samples_channels(prefix);
    engine.seek_to_sample(seek_sample);
    let before = engine.shared.play_pos.load(Ordering::Relaxed);
    engine.replace_samples_keep_pos(Arc::new(neowaves::audio::AudioBuffer::from_channels(full)));
    let after = engine.shared.play_pos.load(Ordering::Relaxed);

    assert_eq!(
        after, before,
        "playhead should not jump during prefix->full handoff (audible re-start risk)"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
