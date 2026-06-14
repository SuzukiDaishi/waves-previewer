//! Regression coverage: destructive editor edits to a virtual item (e.g. a
//! recording that was trimmed in the editor) must survive a session
//! save → reload roundtrip. Previously the session stored a stale snapshot of
//! `virtual_audio` and rebuilt audio from source + op_chain on restore, which
//! only understands `Trim` and ignored in-place destructive edits.
#![cfg(feature = "kittest")]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use egui_kittest::Harness;
use neowaves::kittest::harness_with_startup;
use neowaves::{StartupConfig, WavesPreviewer};

fn make_temp_dir(tag: &str) -> PathBuf {
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let seq = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let mut dir = std::env::temp_dir();
    dir.push(format!(
        "neowaves_session_vedit_{tag}_{}_{}_{}",
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

fn harness_with_folder(dir: PathBuf) -> Harness<'static, WavesPreviewer> {
    let mut cfg = StartupConfig::default();
    cfg.open_folder = Some(dir);
    cfg.open_first = false;
    harness_with_startup(cfg)
}

fn wait_for_tab_ready(harness: &mut Harness<'static, WavesPreviewer>) {
    let start = Instant::now();
    loop {
        harness.run_steps(1);
        let ready = harness
            .state()
            .active_tab
            .and_then(|idx| harness.state().tabs.get(idx))
            .map(|tab| {
                tab.samples_len > 0 && (!tab.loading || harness.state().test_audio_has_samples())
            })
            .unwrap_or(false);
        if ready {
            break;
        }
        if start.elapsed() > Duration::from_secs(20) {
            panic!("tab ready timeout");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn write_recording_wav(dir: &Path, secs: f32) -> PathBuf {
    let path = dir.join("nwcache_recording_take.wav");
    let chans = synth_stereo(48_000, secs);
    neowaves::wave::export_channels_audio(&chans, 48_000, &path).expect("write recording wav");
    path
}

#[test]
fn destructive_trim_on_recording_survives_session_roundtrip() {
    let dir = make_temp_dir("trim_roundtrip");
    let rec = write_recording_wav(&dir, 2.0);
    let full_len = 96_000usize; // 2.0s * 48 kHz

    let mut harness = harness_with_folder(dir.clone());
    harness.run_steps(3);

    // Open the recording as a virtual item and destructively trim it in-place.
    harness.state_mut().test_set_last_recording_path(&rec);
    harness
        .state_mut()
        .test_open_recording_in_editor()
        .expect("recording opens as virtual editor tab");
    wait_for_tab_ready(&mut harness);
    assert!(harness.state_mut().test_apply_trim_frac(0.2, 0.8));
    harness.run_steps(2);

    // The destructively trimmed audio lives in the editor tab buffer.
    let edited_len = harness.state().test_tab_samples_len();
    assert!(
        edited_len > 0 && (edited_len as f32) < (full_len as f32) * 0.8,
        "destructive trim should shorten the editor buffer (edited_len={edited_len}, full={full_len})"
    );

    // Save the session, then mutate + reload it.
    let session_path = dir.join("edit.nwsess");
    assert!(harness.state_mut().test_save_session_to(&session_path));
    harness.run_steps(2);

    assert!(harness.state_mut().test_open_session_from(&session_path));
    harness.run_steps(3);
    // Allow any virtual restore decode to settle.
    for _ in 0..30 {
        if harness.state().test_first_virtual_audio_len().is_some() {
            break;
        }
        harness.run_steps(1);
        std::thread::sleep(Duration::from_millis(20));
    }

    let restored_len = harness
        .state()
        .test_first_virtual_audio_len()
        .expect("virtual item should be restored");
    let ratio = restored_len as f32 / edited_len as f32;
    assert!(
        (0.95..1.05).contains(&ratio),
        "session restore must preserve the destructively-trimmed audio length \
         (edited_len={edited_len}, restored_len={restored_len}, ratio={ratio:.3})"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
