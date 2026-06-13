//! End-to-end regression coverage for the "record → open in editor → trim →
//! save → change volume → save again" workflow.
//!
//! These exercise the recording-produced `(virtual)` item path which used to
//! break on the *second* save after the first save converted the virtual item
//! into a real file on disk.
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
        "neowaves_recording_edit_save_{tag}_{}_{}_{}",
        std::process::id(),
        now_ms,
        seq
    ));
    std::fs::create_dir_all(&dir).expect("create temp test dir");
    dir
}

fn synth_stereo(sr: u32, secs: f32, freq_l: f32, freq_r: f32) -> Vec<Vec<f32>> {
    let frames = ((sr as f32) * secs).max(1.0) as usize;
    let mut left = Vec::with_capacity(frames);
    let mut right = Vec::with_capacity(frames);
    for i in 0..frames {
        let t = (i as f32) / (sr as f32);
        left.push((t * freq_l * std::f32::consts::TAU).sin() * 0.30);
        right.push((t * freq_r * std::f32::consts::TAU).sin() * 0.25);
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

fn wait_for_scan(harness: &mut Harness<'static, WavesPreviewer>) {
    let start = Instant::now();
    loop {
        harness.run_steps(1);
        if !harness.state().files.is_empty() {
            break;
        }
        if start.elapsed() > Duration::from_secs(20) {
            panic!("scan timeout");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn newest_file_with_ext(dir: &Path, ext: &str) -> Option<PathBuf> {
    let mut latest: Option<(SystemTime, PathBuf)> = None;
    for ent in std::fs::read_dir(dir).ok()? {
        let ent = ent.ok()?;
        let p = ent.path();
        let matches = p
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s.eq_ignore_ascii_case(ext))
            .unwrap_or(false);
        if !matches {
            continue;
        }
        let modified = ent
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        match &latest {
            Some((ts, _)) if modified <= *ts => {}
            _ => latest = Some((modified, p)),
        }
    }
    latest.map(|(_, p)| p)
}

fn wait_for_export_finish(harness: &mut Harness<'static, WavesPreviewer>) {
    let start = Instant::now();
    loop {
        harness.run_steps(1);
        if !harness.state().test_export_in_progress() {
            break;
        }
        if start.elapsed() > Duration::from_secs(30) {
            panic!("export timeout");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn count_files_with_ext(dir: &Path, ext: &str) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .filter(|e| {
            e.path()
                .extension()
                .and_then(|s| s.to_str())
                .map(|s| s.eq_ignore_ascii_case(ext))
                .unwrap_or(false)
        })
        .count()
}

fn write_recording_wav(dir: &Path) -> PathBuf {
    let path = dir.join("nwcache_recording_take.wav");
    let chans = synth_stereo(48_000, 4.0, 220.0, 440.0);
    neowaves::wave::export_channels_audio(&chans, 48_000, &path).expect("write recording wav");
    path
}

/// Writes a "recording" into the real NeoWaves temp cache dir, mirroring where
/// `start_recording` allocates its temp WAV. Used to verify saves don't bury
/// output in the OS temp dir.
fn write_recording_in_neowaves_temp(tag: &str) -> PathBuf {
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    let seq = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join("NeoWaves").join("recording");
    std::fs::create_dir_all(&dir).expect("create neowaves temp recording dir");
    let path = dir.join(format!(
        "nwcache_{}_{}_{}_rec.wav",
        std::process::id(),
        tag,
        seq
    ));
    let chans = synth_stereo(48_000, 4.0, 220.0, 440.0);
    neowaves::wave::export_channels_audio(&chans, 48_000, &path).expect("write recording wav");
    path
}

fn peak_abs(path: &Path) -> f32 {
    let (chans, _sr) =
        neowaves::audio_io::decode_audio_multi(path).expect("decode exported wav for peak");
    chans
        .iter()
        .flat_map(|c| c.iter())
        .fold(0.0f32, |acc, &v| acc.max(v.abs()))
}

/// Reproduces the reported bug: after recording, trim + save once, then change
/// the volume and save again. The second save must also produce a file.
#[test]
fn recording_trim_save_then_volume_save_again() {
    let dir = make_temp_dir("trim_save_volume");
    let rec = write_recording_wav(&dir);
    let export_dir = dir.join("exports");
    std::fs::create_dir_all(&export_dir).expect("create export dir");

    let mut harness = harness_with_folder(dir.clone());
    harness.run_steps(3);

    // Simulate "Open in Editor" for a freshly finalized recording.
    harness.state_mut().test_set_last_recording_path(&rec);
    let virtual_path = harness
        .state_mut()
        .test_open_recording_in_editor()
        .expect("recording should open as a virtual editor tab");
    wait_for_tab_ready(&mut harness);

    // --- Trim in the editor ---
    assert!(
        harness.state_mut().test_apply_trim_frac(0.2, 0.8),
        "trim should apply to the recorded virtual item"
    );
    harness.run_steps(2);

    // --- First save (New File) ---
    harness.state_mut().test_switch_to_list();
    assert!(harness.state_mut().test_select_path(&virtual_path));
    harness.state_mut().test_set_export_first_prompt(false);
    harness.state_mut().test_set_export_save_mode_overwrite(false);
    harness.state_mut().test_set_export_conflict("rename");
    harness
        .state_mut()
        .test_set_export_dest_folder(Some(&export_dir));
    harness
        .state_mut()
        .test_set_export_name_template("recording_take");
    harness.state_mut().test_trigger_save_selected();
    wait_for_export_finish(&mut harness);
    harness.run_steps(3);

    assert_eq!(
        count_files_with_ext(&export_dir, "wav"),
        1,
        "first save should produce exactly one exported wav"
    );

    // The item that was virtual is now a real file on disk. Grab its path.
    let saved_path = harness
        .state()
        .test_selected_path()
        .cloned()
        .expect("an item should remain selected after the first save");

    // --- Change the volume in the editor, then save again ---
    harness.state_mut().test_open_tab_for_path(&saved_path);
    wait_for_tab_ready(&mut harness);
    assert!(
        harness.state_mut().test_apply_gain(0.0, 1.0, -6.0),
        "gain (volume) change should apply to the saved recording"
    );
    harness.run_steps(2);

    harness.state_mut().test_switch_to_list();
    assert!(harness.state_mut().test_select_path(&saved_path));
    harness.state_mut().test_trigger_save_selected();
    wait_for_export_finish(&mut harness);
    harness.run_steps(3);

    assert_eq!(
        count_files_with_ext(&export_dir, "wav"),
        2,
        "second save after a volume change must also produce a file"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Overwrite-mode re-save: the natural "save, tweak volume, save again over the
/// same file" flow. The single saved file must end up reflecting the lowered
/// volume (peak roughly halved by -6 dB).
#[test]
fn recording_trim_save_overwrite_then_volume_overwrite() {
    let dir = make_temp_dir("overwrite_revolume");
    let rec = write_recording_wav(&dir);
    let export_dir = dir.join("exports");
    std::fs::create_dir_all(&export_dir).expect("create export dir");

    let mut harness = harness_with_folder(dir.clone());
    harness.run_steps(3);

    harness.state_mut().test_set_last_recording_path(&rec);
    let virtual_path = harness
        .state_mut()
        .test_open_recording_in_editor()
        .expect("recording should open as a virtual editor tab");
    wait_for_tab_ready(&mut harness);

    assert!(harness.state_mut().test_apply_trim_frac(0.2, 0.8));
    harness.run_steps(2);

    // First save (Overwrite mode, name template without gain so the file name
    // is stable across re-saves).
    harness.state_mut().test_switch_to_list();
    assert!(harness.state_mut().test_select_path(&virtual_path));
    harness.state_mut().test_set_export_first_prompt(false);
    harness.state_mut().test_set_export_save_mode_overwrite(true);
    harness.state_mut().test_set_export_conflict("overwrite");
    harness
        .state_mut()
        .test_set_export_dest_folder(Some(&export_dir));
    harness.state_mut().test_set_export_name_template("take");
    harness.state_mut().test_trigger_save_selected();
    wait_for_export_finish(&mut harness);
    harness.run_steps(3);

    assert_eq!(
        count_files_with_ext(&export_dir, "wav"),
        1,
        "overwrite save should produce exactly one file"
    );
    let saved_path = harness
        .state()
        .test_selected_path()
        .cloned()
        .expect("item should remain selected after the first save");
    let peak_before = peak_abs(&saved_path);
    assert!(peak_before > 0.2, "recorded peak should be audible");

    // Change the volume in the editor and overwrite-save again.
    harness.state_mut().test_open_tab_for_path(&saved_path);
    wait_for_tab_ready(&mut harness);
    assert!(harness.state_mut().test_apply_gain(0.0, 1.0, -6.0));
    harness.run_steps(2);

    harness.state_mut().test_switch_to_list();
    assert!(harness.state_mut().test_select_path(&saved_path));
    harness.state_mut().test_trigger_save_selected();
    wait_for_export_finish(&mut harness);
    harness.run_steps(3);

    assert_eq!(
        count_files_with_ext(&export_dir, "wav"),
        1,
        "overwrite re-save must not spawn a second file"
    );
    let peak_after = peak_abs(&saved_path);
    let ratio = peak_after / peak_before;
    assert!(
        (0.40..0.60).contains(&ratio),
        "overwrite re-save must apply the -6 dB volume change \
         (peak_before={peak_before:.4} peak_after={peak_after:.4} ratio={ratio:.3})"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A virtual clip trimmed from a 16-bit source must export as 16-bit WAV
/// (matching the source / list metadata), not be silently upgraded to 32-bit
/// float.
#[test]
fn virtual_trim_from_16bit_source_exports_16bit() {
    let dir = make_temp_dir("vbits16");
    let src = dir.join("src16.wav");
    let chans = synth_stereo(48_000, 2.0, 220.0, 440.0);
    neowaves::wave::export_channels_audio_with_depth(
        &chans,
        48_000,
        &src,
        Some(neowaves::wave::WavBitDepth::Pcm16),
    )
    .expect("write 16-bit source");
    assert_eq!(
        neowaves::audio_io::read_audio_info(&src)
            .map(|i| i.bits_per_sample)
            .unwrap_or(0),
        16
    );
    let export_dir = dir.join("exports");
    std::fs::create_dir_all(&export_dir).expect("create export dir");

    let mut harness = harness_with_folder(dir.clone());
    wait_for_scan(&mut harness);
    assert!(harness.state_mut().test_open_first_tab());
    wait_for_tab_ready(&mut harness);
    // Non-destructive "add trim as virtual" carries the source bit depth.
    assert!(harness.state_mut().test_add_trim_virtual_frac(0.2, 0.7));
    harness.run_steps(3);
    harness.state_mut().test_switch_to_list();

    let virtual_path = harness
        .state()
        .test_selected_path()
        .cloned()
        .expect("virtual item selected");
    assert!(harness.state_mut().test_select_path(&virtual_path));
    harness.state_mut().test_set_export_first_prompt(false);
    harness.state_mut().test_set_export_save_mode_overwrite(false);
    harness.state_mut().test_set_export_conflict("rename");
    harness
        .state_mut()
        .test_set_export_dest_folder(Some(&export_dir));
    harness.state_mut().test_set_export_name_template("clip16");
    harness.state_mut().test_trigger_save_selected();
    wait_for_export_finish(&mut harness);
    harness.run_steps(3);

    let out = newest_file_with_ext(&export_dir, "wav").expect("missing exported wav");
    let bits = neowaves::audio_io::read_audio_info(&out)
        .map(|i| i.bits_per_sample)
        .unwrap_or(0);
    assert_eq!(
        bits, 16,
        "virtual clip from a 16-bit source should export as 16-bit (got {bits})"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// After a recording is saved to a real file, the editor tab must not be left
/// in a phantom "dirty / unsaved edits" state: the on-disk file already matches
/// the buffer.
#[test]
fn recording_save_leaves_clean_editor_tab() {
    let dir = make_temp_dir("clean_after_save");
    let rec = write_recording_wav(&dir);
    let export_dir = dir.join("exports");
    std::fs::create_dir_all(&export_dir).expect("create export dir");

    let mut harness = harness_with_folder(dir.clone());
    harness.run_steps(3);

    harness.state_mut().test_set_last_recording_path(&rec);
    let virtual_path = harness
        .state_mut()
        .test_open_recording_in_editor()
        .expect("recording should open as a virtual editor tab");
    wait_for_tab_ready(&mut harness);

    assert!(harness.state_mut().test_apply_trim_frac(0.2, 0.8));
    harness.run_steps(2);
    assert!(
        harness.state().test_tab_dirty(),
        "tab should be dirty after trimming but before saving"
    );

    harness.state_mut().test_switch_to_list();
    assert!(harness.state_mut().test_select_path(&virtual_path));
    harness.state_mut().test_set_export_first_prompt(false);
    harness.state_mut().test_set_export_save_mode_overwrite(false);
    harness.state_mut().test_set_export_conflict("rename");
    harness
        .state_mut()
        .test_set_export_dest_folder(Some(&export_dir));
    harness.state_mut().test_set_export_name_template("take");
    harness.state_mut().test_trigger_save_selected();
    wait_for_export_finish(&mut harness);
    harness.run_steps(3);

    let saved_path = harness
        .state()
        .test_selected_path()
        .cloned()
        .expect("item should remain selected after the save");
    harness.state_mut().test_open_tab_for_path(&saved_path);
    harness.run_steps(2);
    assert!(
        !harness.state().test_tab_dirty(),
        "after saving the recording to disk the editor tab must be clean \
         (no phantom unsaved-edits state)"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// When no explicit destination folder is configured, a saved recording must
/// land in the currently open folder rather than being buried in the OS temp
/// directory (where the recording's working file lives and where it is subject
/// to temp cleanup).
#[test]
fn recording_save_without_dest_goes_to_open_folder_not_temp() {
    let root = make_temp_dir("save_to_root");
    let rec = write_recording_in_neowaves_temp("save_to_root");

    let mut harness = harness_with_folder(root.clone());
    harness.run_steps(3);

    harness.state_mut().test_set_last_recording_path(&rec);
    let virtual_path = harness
        .state_mut()
        .test_open_recording_in_editor()
        .expect("recording should open as a virtual editor tab");
    wait_for_tab_ready(&mut harness);
    assert!(harness.state_mut().test_apply_trim_frac(0.2, 0.8));
    harness.run_steps(2);

    harness.state_mut().test_switch_to_list();
    assert!(harness.state_mut().test_select_path(&virtual_path));
    harness.state_mut().test_set_export_first_prompt(false);
    harness.state_mut().test_set_export_save_mode_overwrite(false);
    harness.state_mut().test_set_export_conflict("rename");
    harness.state_mut().test_set_export_dest_folder(None); // no explicit dest
    harness.state_mut().test_set_export_name_template("my_take");
    harness.state_mut().test_trigger_save_selected();
    wait_for_export_finish(&mut harness);
    harness.run_steps(3);

    let saved_path = harness
        .state()
        .test_selected_path()
        .cloned()
        .expect("item should remain selected after the save");
    assert_eq!(
        saved_path.parent(),
        Some(root.as_path()),
        "saved recording should live in the open folder, not the temp dir \
         (got {})",
        saved_path.display()
    );
    assert!(saved_path.is_file(), "saved file should exist on disk");

    let _ = std::fs::remove_file(&rec);
    let _ = std::fs::remove_dir_all(&root);
}

/// Markers and a loop region added to a recording in the editor must survive the
/// save, so the saved file is a first-class audio file (and the editor tab is
/// reported clean afterwards).
#[test]
fn recording_save_persists_markers_and_loop() {
    let dir = make_temp_dir("markers_loop");
    let rec = write_recording_wav(&dir);
    let export_dir = dir.join("exports");
    std::fs::create_dir_all(&export_dir).expect("create export dir");

    let mut harness = harness_with_folder(dir.clone());
    harness.run_steps(3);

    harness.state_mut().test_set_last_recording_path(&rec);
    let virtual_path = harness
        .state_mut()
        .test_open_recording_in_editor()
        .expect("recording should open as a virtual editor tab");
    wait_for_tab_ready(&mut harness);

    assert!(harness.state_mut().test_add_marker_frac(0.5));
    harness.run_steps(2);

    harness.state_mut().test_switch_to_list();
    assert!(harness.state_mut().test_select_path(&virtual_path));
    harness.state_mut().test_set_export_first_prompt(false);
    harness.state_mut().test_set_export_save_mode_overwrite(false);
    harness.state_mut().test_set_export_conflict("rename");
    harness
        .state_mut()
        .test_set_export_dest_folder(Some(&export_dir));
    harness.state_mut().test_set_export_name_template("marked");
    harness.state_mut().test_trigger_save_selected();
    wait_for_export_finish(&mut harness);
    harness.run_steps(3);

    let saved_path = harness
        .state()
        .test_selected_path()
        .cloned()
        .expect("item should remain selected after the save");
    let saved_markers =
        neowaves::markers::read_markers(&saved_path, 48_000, 48_000).unwrap_or_default();
    assert!(
        !saved_markers.is_empty(),
        "marker added in the editor must be written to the saved recording"
    );

    // Editor tab should be clean (audio + markers persisted).
    harness.state_mut().test_open_tab_for_path(&saved_path);
    harness.run_steps(2);
    assert!(
        !harness.state().test_tab_dirty() && !harness.state().test_marker_dirty(),
        "saved recording with markers must leave a clean editor tab"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
