//! Multichannel material (5.1, 7.1.4, ...): see `docs/MULTICHANNEL_SPEC.md`.
#![cfg(feature = "kittest")]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use egui_kittest::kittest::{NodeT, Queryable};
use egui_kittest::Harness;
use neowaves::app::ViewMode;
use neowaves::kittest::{harness_with_startup, harness_with_startup_scaled};
use neowaves::{StartupConfig, WavesPreviewer};

type App = Harness<'static, WavesPreviewer>;

/// The rate the fixtures are written at.
const FIXTURE_SR: u32 = 48_000;

fn temp_dir(tag: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "neowaves_multichannel_{tag}_{}_{now}_{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

/// `channels` channels of `secs` seconds, each a tone of its own pitch.
fn write_multichannel(path: &Path, channels: usize, secs: f32) {
    let frames = (FIXTURE_SR as f32 * secs) as usize;
    let chans: Vec<Vec<f32>> = (0..channels)
        .map(|c| {
            let freq = 220.0 * (1.0 + c as f32 * 0.25);
            (0..frames)
                .map(|i| (i as f32 / FIXTURE_SR as f32 * freq * std::f32::consts::TAU).sin() * 0.2)
                .collect()
        })
        .collect();
    neowaves::wave::export_channels_audio(&chans, FIXTURE_SR, path).expect("write fixture");
}

/// One tone per channel, channel `c` at amplitude `amps[c]`.
fn write_levels(path: &Path, amps: &[f32], secs: f32) {
    let frames = (FIXTURE_SR as f32 * secs) as usize;
    let chans: Vec<Vec<f32>> = amps
        .iter()
        .enumerate()
        .map(|(c, amp)| {
            let freq = 220.0 * (1.0 + c as f32 * 0.25);
            (0..frames)
                .map(|i| (i as f32 / FIXTURE_SR as f32 * freq * std::f32::consts::TAU).sin() * amp)
                .collect()
        })
        .collect();
    neowaves::wave::export_channels_audio(&chans, FIXTURE_SR, path).expect("write fixture");
}

/// Put the playhead mid-file so the meters have a full window, and let
/// them settle.
fn settle_meters(harness: &mut App, at_secs: f32) {
    harness
        .state_mut()
        .test_audio_seek_to_sample((FIXTURE_SR as f32 * at_secs) as usize);
    for _ in 0..20 {
        harness.run_steps(1);
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn wait_until(harness: &mut App, what: &str, secs: u64, mut done: impl FnMut(&mut App) -> bool) {
    let start = Instant::now();
    while !done(harness) {
        assert!(
            start.elapsed() < Duration::from_secs(secs),
            "timed out waiting for {what}"
        );
        harness.run_steps(1);
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn open_in_editor(dir: &Path, file: &Path) -> App {
    open_in_editor_at(dir, file, None)
}

/// As `open_in_editor`, on a display scaled by `pixels_per_point`.
fn open_in_editor_at(dir: &Path, file: &Path, pixels_per_point: Option<f32>) -> App {
    let cfg = StartupConfig {
        open_folder: Some(dir.to_path_buf()),
        open_first: false,
        ..StartupConfig::default()
    };
    let mut harness = match pixels_per_point {
        Some(ppp) => harness_with_startup_scaled(cfg, egui::vec2(1280.0, 720.0), ppp),
        None => harness_with_startup(cfg),
    };
    wait_until(&mut harness, "the scan", 15, |h| {
        !h.state().scan_in_progress && !h.state().files.is_empty()
    });
    assert!(harness.state_mut().test_open_tab_for_path(file));
    wait_until(&mut harness, "the tab", 30, |h| {
        h.state()
            .active_tab
            .and_then(|idx| h.state().tabs.get(idx))
            .is_some_and(|tab| tab.samples_len > 0 && !tab.loading)
    });
    harness
}

/// A 16-bit WAVE_FORMAT_EXTENSIBLE file whose header names its speakers
/// with `mask` (one bit per channel, in channel order).
fn write_wav_with_mask(path: &Path, channels: u16, mask: u32, secs: f32) {
    let frames = (FIXTURE_SR as f32 * secs) as u32;
    let block_align = channels * 2;
    let data_len = frames * block_align as u32;
    let mut bytes = Vec::with_capacity(68 + data_len as usize);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(60 + data_len).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&40u32.to_le_bytes());
    bytes.extend_from_slice(&0xFFFEu16.to_le_bytes());
    bytes.extend_from_slice(&channels.to_le_bytes());
    bytes.extend_from_slice(&FIXTURE_SR.to_le_bytes());
    bytes.extend_from_slice(&(FIXTURE_SR * block_align as u32).to_le_bytes());
    bytes.extend_from_slice(&block_align.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(&22u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(&mask.to_le_bytes());
    // KSDATAFORMAT_SUBTYPE_PCM
    bytes.extend_from_slice(&[
        0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B,
        0x71,
    ]);
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    for i in 0..frames {
        for c in 0..channels {
            let freq = 220.0 * (1.0 + c as f32 * 0.25);
            let v = (i as f32 / FIXTURE_SR as f32 * freq * std::f32::consts::TAU).sin() * 0.2;
            bytes.extend_from_slice(&((v * i16::MAX as f32) as i16).to_le_bytes());
        }
    }
    std::fs::write(path, bytes).expect("write the masked wav");
}

fn wait_for_meta(harness: &mut App, files: &[PathBuf]) {
    let files = files.to_vec();
    wait_until(harness, "metadata", 30, |h| {
        files.iter().all(|f| h.state().test_meta_loaded(f))
    });
}

/// Stereo white noise (a fixed sequence): a spectrum filled edge to edge.
#[cfg(feature = "kittest_render")]
fn write_noise(path: &Path, secs: f32) {
    let frames = (FIXTURE_SR as f32 * secs) as usize;
    let mut state = 0x2545_f491_u32;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        (state as f32 / u32::MAX as f32 * 2.0 - 1.0) * 0.3
    };
    let chans: Vec<Vec<f32>> = (0..2)
        .map(|_| (0..frames).map(|_| next()).collect())
        .collect();
    neowaves::wave::export_channels_audio(&chans, FIXTURE_SR, path).expect("write noise");
}

/// At display scales that are not whole numbers the spectrum used to show
/// dark vertical seams where per-column rects met mid-pixel. Scans a row
/// just above its bottom (below the frequency labels) for a pixel darker
/// than both of its neighbours, skipping the frequency tick marks.
#[cfg(feature = "kittest_render")]
#[test]
fn kittest_render_the_spectrum_has_no_seams_at_fractional_scales() {
    let dir = temp_dir("spectrum_seams");
    let file = dir.join("noise.wav");
    write_noise(&file, 3.0);
    let out_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("debug")
        .join("screenshot_verify")
        .join("multichannel");
    std::fs::create_dir_all(&out_dir).expect("create evidence dir");
    for (ppp, name) in [(1.25f32, "spectrum_125.png"), (1.5, "spectrum_150.png")] {
        let mut harness = open_in_editor_at(&dir, &file, Some(ppp));
        // Mid-file, so the analyzer's window is full of noise.
        harness
            .state_mut()
            .test_audio_seek_to_sample(FIXTURE_SR as usize * 3 / 2);
        for _ in 0..20 {
            harness.run_steps(1);
            std::thread::sleep(Duration::from_millis(20));
        }
        let rect = harness
            .state()
            .test_mini_meter_spectrum_rect()
            .expect("the spectrum was drawn");
        let image = harness.render().expect("render");
        image.save(out_dir.join(name)).expect("save the screenshot");
        // The tick marks at 50 / 100 / 1k / 10k Hz, as the meter places them.
        let (f_lo, f_hi) = (20.0f32, FIXTURE_SR as f32 * 0.5);
        let ticks: Vec<f32> = [50.0f32, 100.0, 1_000.0, 10_000.0]
            .iter()
            .map(|f| (rect.left() + (f / f_lo).ln() / (f_hi / f_lo).ln() * rect.width()) * ppp)
            .collect();
        let y = ((rect.bottom() - 2.5) * ppp) as u32;
        let (x0, x1) = (
            ((rect.left() + 4.0) * ppp) as u32,
            ((rect.right() - 4.0) * ppp) as u32,
        );
        let lum = |x: u32| {
            let p = image.get_pixel(x, y);
            0.299 * p[0] as f32 + 0.587 * p[1] as f32 + 0.114 * p[2] as f32
        };
        let scanned: Vec<u32> = (x0 + 1..x1 - 1)
            .filter(|&x| ticks.iter().all(|t| (x as f32 - t).abs() > 4.0))
            .collect();
        // Not an empty panel (its background is about 18): the spectrum is
        // drawn along the whole row.
        let filled = scanned.iter().filter(|&&x| lum(x) > 45.0).count();
        assert!(
            filled * 10 >= scanned.len() * 9,
            "the spectrum fills the row at scale {ppp}: {filled} of {}",
            scanned.len()
        );
        let seams: Vec<u32> = scanned
            .into_iter()
            .filter(|&x| lum(x) < 0.85 * lum(x - 1).min(lum(x + 1)))
            .collect();
        assert!(seams.is_empty(), "dark seams at scale {ppp}: {seams:?}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_layout_comes_from_the_file_then_its_mask_then_the_default_then_the_standard() {
    let dir = temp_dir("layout_order");
    // 3.1 by its mask; the standard order for four channels is quad.
    let masked = dir.join("three_one.wav");
    write_wav_with_mask(&masked, 4, 0xF, 0.5);
    // No mask at all (as ffmpeg writes for an unnamed layout).
    let plain = dir.join("five_one.wav");
    write_wav_with_mask(&plain, 6, 0, 0.5);
    let cfg = StartupConfig {
        open_folder: Some(dir.clone()),
        open_first: false,
        ..StartupConfig::default()
    };
    let mut harness = harness_with_startup(cfg);
    wait_until(&mut harness, "the scan", 15, |h| h.state().files.len() == 2);
    wait_for_meta(&mut harness, &[masked.clone(), plain.clone()]);
    let layout =
        |h: &App, path: &Path, n: usize| h.state().test_channel_layout(path, n).expect("a layout");

    assert_eq!(
        layout(&harness, &masked, 4),
        (
            "FL,FR,FC,LFE".to_string(),
            "from the file's channel mask".to_string()
        )
    );
    assert_eq!(
        layout(&harness, &plain, 6),
        (
            "FL,FR,FC,LFE,BL,BR".to_string(),
            "standard WAV order".to_string()
        )
    );
    // A file's own choice wins over everything.
    let film = "FL,FC,FR,BL,BR,LFE";
    assert!(harness
        .state_mut()
        .test_set_channel_layout_override(&plain, Some(film)));
    assert_eq!(
        layout(&harness, &plain, 6),
        (film.to_string(), "chosen for this file".to_string())
    );
    assert!(harness
        .state_mut()
        .test_set_channel_layout_override(&masked, Some("FL,FR,BL,BR")));
    assert_eq!(
        layout(&harness, &masked, 4).0,
        "FL,FR,BL,BR",
        "over the mask too"
    );
    // Forgotten again, the mask is back.
    assert!(harness
        .state_mut()
        .test_set_channel_layout_override(&masked, None));
    assert_eq!(layout(&harness, &masked, 4).0, "FL,FR,FC,LFE");

    // The session keeps the file's choice.
    let session = dir.join("layouts.nwsess");
    assert!(harness.state_mut().test_save_session_to(&session));
    let mut reopened = neowaves::kittest::harness_default();
    reopened.run_steps(2);
    assert!(reopened.state_mut().test_open_session_from(&session));
    assert_eq!(
        reopened
            .state()
            .test_channel_layout(&plain, 6)
            .expect("a layout")
            .0,
        film,
        "restored from the session"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn playback_routes_by_the_layout_and_the_window_saves_a_default() {
    let dir = temp_dir("layout_playback");
    // No channel mask, so nothing outranks the default saved below.
    let file = dir.join("film_order.wav");
    write_wav_with_mask(&file, 6, 0, 0.5);
    let mut harness = open_in_editor(&dir, &file);
    harness.run_steps(4);
    assert_eq!(
        harness.state().test_playback_source_layout().as_deref(),
        Some("FL,FR,FC,LFE,BL,BR"),
        "the standard order until told otherwise"
    );
    assert!(harness
        .state_mut()
        .test_set_channel_layout_override(&file, Some("FL,FC,FR,BL,BR,LFE")));
    harness.run_steps(2);
    assert_eq!(
        harness.state().test_playback_source_layout().as_deref(),
        Some("FL,FC,FR,BL,BR,LFE"),
        "playback follows the file's layout"
    );

    // The window, from the meter's menu: saved as the six-channel default.
    assert!(harness
        .state_mut()
        .test_set_channel_layout_override(&file, None));
    assert!(harness.state_mut().test_request_channel_layout_editor());
    harness.run_steps(3);
    assert!(harness.state().test_channel_layout_editor_open());
    harness.get_by_label("Default for 6 channels").click();
    harness.run_steps(3);
    assert!(
        !harness.state().test_channel_layout_editor_open(),
        "closed once saved"
    );
    assert_eq!(
        harness
            .state()
            .test_channel_layout(&file, 6)
            .expect("a layout")
            .1,
        "your default for this channel count"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn headphones_render_a_surround_file_through_the_hrtf() {
    let dir = temp_dir("hrtf");
    let file = dir.join("five_one.wav");
    write_wav_with_mask(&file, 6, 0x3F, 1.0);
    let mut harness = open_in_editor(&dir, &file);
    harness.run_steps(4);
    assert_eq!(
        harness.state().test_binaural_channels(),
        None,
        "off until asked"
    );

    // The top bar's toggle: the bundled HRTF loads and takes the 5.1.
    harness.get_by_label("HRTF").click();
    wait_until(&mut harness, "the bundled HRTF", 60, |h| {
        h.state().test_binaural_channels() == Some(6)
    });
    assert_eq!(harness.state().test_hrtf_status(), "Active { channels: 6 }");

    // The window, from the meter's menu. Another room is another set of
    // filters, built at once.
    assert!(harness.state_mut().test_request_hrtf_window());
    harness.run_steps(3);
    assert!(harness.state().test_hrtf_window_open());
    let before = harness.state().test_binaural_filters_id();
    // A combo box carries its choice as its value, not its label.
    harness
        .query_all_by_value("Standard (ITU / Dolby)")
        .find(|node| node.accesskit_node().role() == egui::accesskit::Role::ComboBox)
        .expect("the room combo")
        .click();
    harness.run_steps(2);
    harness.get_by_label("Quad (square)").click();
    harness.run_steps(3);
    assert_eq!(harness.state().test_hrtf_room(), "quad");
    assert_ne!(
        harness.state().test_binaural_filters_id(),
        before,
        "new room, new filters"
    );

    // A SOFA of the user's: added, switched to, loaded, used.
    let other = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("assets/sofas/D1_HRIR_SOFA/D1_44K_16bit_256tap_FIR_SOFA.sofa");
    harness.state_mut().test_queue_sofa_dialog(Some(other));
    harness.get_by_label("Load SOFA\u{2026}").click();
    wait_until(&mut harness, "the added SOFA", 60, |h| {
        h.state().test_hrtf_profile_label() == "D1_44K_16bit_256tap_FIR_SOFA.sofa"
            && h.state().test_hrtf_status() == "Active { channels: 6 }"
    });

    // Off again, from the window's own toggle: back to the speakers.
    harness.get_by_label("HRTF On").click();
    harness.run_steps(3);
    assert_eq!(harness.state().test_binaural_channels(), None);
    assert_eq!(harness.state().test_hrtf_status(), "Off");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Stereo goes through the HRTF too, from the L and R speakers, unless
/// the top bar's menu says it should go straight to the ears.
#[test]
fn headphones_play_stereo_from_the_l_and_r_speakers() {
    let dir = temp_dir("hrtf_stereo");
    let file = dir.join("stereo.wav");
    write_levels(&file, &[0.3, 0.1], 1.0);
    let mut harness = open_in_editor(&dir, &file);
    harness.run_steps(4);
    harness.get_by_label("HRTF").click();
    wait_until(&mut harness, "the bundled HRTF", 60, |h| {
        h.state().test_binaural_channels() == Some(2)
    });
    assert_eq!(harness.state().test_hrtf_status(), "Active { channels: 2 }");

    harness.get_by_label("HRTF").click_secondary();
    harness.run_steps(2);
    harness.get_by_label("Stereo from L / R speakers").click();
    harness.run_steps(3);
    assert_eq!(harness.state().test_binaural_channels(), None);
    assert_eq!(
        harness.state().test_hrtf_status(),
        "Bypassed(\"stereo plays directly\")"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A 2.1 file is read as L R LFE from its mask, and saving it keeps that
/// mask -- hound on its own would write LCR.
#[test]
fn a_two_one_file_keeps_its_mask_when_saved() {
    let dir = temp_dir("two_one");
    let file = dir.join("two_one.wav");
    write_wav_with_mask(&file, 3, 0x0B, 1.0);
    let mut harness = open_in_editor(&dir, &file);
    wait_for_meta(&mut harness, &[file.clone()]);
    assert_eq!(
        harness
            .state()
            .test_channel_layout(&file, 3)
            .expect("a layout")
            .0,
        "FL,FR,LFE"
    );
    harness.state_mut().test_set_export_first_prompt(false);
    harness.state_mut().test_set_export_save_mode_overwrite(true);
    harness.state_mut().test_set_pending_gain_db_for_path(&file, -3.0);
    assert!(harness.state_mut().test_select_paths_multi(&[file.clone()]));
    harness.state_mut().test_trigger_save_selected();
    wait_until(&mut harness, "the save", 30, |h| !h.state().test_export_in_progress());
    let info = neowaves::audio_io::read_audio_info(&file).expect("read the saved file");
    assert_eq!(info.channels, 3);
    assert_eq!(info.channel_mask, Some(0x0B), "still L R LFE after a gain");

    // A conversion rewrites the audio itself, through the edit save.
    harness.run_steps(3);
    assert!(harness
        .state_mut()
        .test_set_selected_sample_rate_override(44_100));
    harness.state_mut().test_trigger_save_selected();
    wait_until(&mut harness, "the second save", 30, |h| !h.state().test_export_in_progress());
    let info = neowaves::audio_io::read_audio_info(&file).expect("read the converted file");
    assert_eq!(info.sample_rate, 44_100);
    assert_eq!(info.channel_mask, Some(0x0B), "still L R LFE after a conversion");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The top bar's output meter has a cell per output of the device, named
/// by the speaker it feeds, whatever is playing -- and empty while nothing is.
#[test]
fn the_output_meter_has_a_cell_per_output_of_the_device() {
    let mut harness = harness_with_startup(StartupConfig::default());
    harness.run_steps(2);
    let names = |h: &App| -> Vec<String> {
        h.state().test_output_meter().into_iter().map(|(name, _)| name).collect()
    };
    assert_eq!(names(&harness), ["L", "R"], "the test device is stereo");

    harness.state_mut().test_use_output_device_channels(6);
    harness.run_steps(2);
    assert_eq!(names(&harness), ["L", "R", "C", "LFE", "Ls", "Rs"]);
    assert!(harness
        .state()
        .test_output_meter()
        .iter()
        .all(|(_, db)| *db <= -79.9), "silent while nothing plays");

    // What the callback measured, output by output: here only C and Rs.
    let mut levels = vec![(0.0f32, 0.0f32); 6];
    levels[2] = (0.5, 0.7);
    levels[5] = (0.1, 0.2);
    harness.state_mut().test_inject_channel_meters(&levels);
    harness.run_steps(2);
    let meter = harness.state().test_output_meter();
    assert!((meter[2].1 - 20.0 * 0.5f32.log10()).abs() < 0.2, "{meter:?}");
    assert!((meter[5].1 - 20.0 * 0.1f32.log10()).abs() < 0.2, "{meter:?}");
    assert!(meter[0].1 <= -79.9 && meter[3].1 <= -79.9, "{meter:?}");

    // Wider than 7.1.4: the first twelve by speaker, the rest by number.
    harness.state_mut().test_use_output_device_channels(16);
    harness.run_steps(2);
    let wide = names(&harness);
    assert_eq!(wide.len(), 16);
    assert_eq!(&wide[..4], ["L", "R", "C", "LFE"]);
    assert_eq!(&wide[12..], ["13", "14", "15", "16"]);
}

#[test]
fn a_five_one_file_gets_the_surround_view_pointing_where_the_sound_is() {
    let dir = temp_dir("surround_view");
    // Only the centre speaker sounds.
    let centre = dir.join("centre_only.wav");
    write_levels(&centre, &[0.0, 0.0, 0.5, 0.0, 0.0, 0.0], 2.0);
    let stereo = dir.join("stereo.wav");
    write_levels(&stereo, &[0.3, 0.3], 2.0);
    let mut harness = open_in_editor(&dir, &centre);
    settle_meters(&mut harness, 1.0);
    let (view, vector) = harness
        .state()
        .test_mini_meter_channel_view()
        .expect("a mini meter");
    assert_eq!(view, "SURROUND");
    let [x, y, z] = vector.expect("sound");
    assert!(
        y > 0.99 && x.abs() < 0.01 && z.abs() < 0.01,
        "straight ahead: {:?}",
        [x, y, z]
    );

    assert!(harness.state_mut().test_open_tab_for_path(&stereo));
    wait_until(&mut harness, "the stereo tab", 30, |h| {
        h.state()
            .active_tab
            .and_then(|idx| h.state().tabs.get(idx))
            .is_some_and(|tab| tab.path == stereo && tab.samples_len > 0 && !tab.loading)
    });
    settle_meters(&mut harness, 1.0);
    assert_eq!(
        harness
            .state()
            .test_mini_meter_channel_view()
            .expect("a mini meter")
            .0,
        "STEREO",
        "two channels keep the vectorscope"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The top bar's output meter on a stereo, a 5.1 and a 7.1.4 device.
#[cfg(feature = "kittest_render")]
#[test]
fn kittest_render_the_output_meter_per_device_width() {
    let mut harness = harness_with_startup(StartupConfig::default());
    harness.set_size(egui::vec2(1280.0, 720.0));
    harness.run_steps(3);
    let out_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("debug")
        .join("screenshot_verify")
        .join("output_meter");
    std::fs::create_dir_all(&out_dir).expect("evidence dir");
    for channels in [2usize, 6, 12] {
        harness.state_mut().test_use_output_device_channels(channels);
        let levels: Vec<(f32, f32)> = (0..channels)
            .map(|i| {
                let rms = 0.05 + 0.6 * ((i * 7 % channels) as f32 / channels as f32);
                (rms, (rms * 1.4).min(1.0))
            })
            .collect();
        for _ in 0..4 {
            harness.state_mut().test_inject_channel_meters(&levels);
            harness.run_steps(1);
        }
        harness
            .render()
            .expect("render")
            .save(out_dir.join(format!("output_meter_{channels}ch.png")))
            .expect("save");
    }
}

/// A 7.1.4 file with L and Ltf loud, Rrs quiet: the SURROUND view.
#[cfg(feature = "kittest_render")]
#[test]
fn kittest_render_the_surround_view_of_a_7_1_4_file() {
    let dir = temp_dir("surround_render");
    let file = dir.join("immersive.wav");
    // L R C LFE Lrs Rrs Lss Rss Ltf Rtf Ltr Rtr
    write_levels(
        &file,
        &[
            0.5, 0.02, 0.05, 0.2, 0.01, 0.06, 0.01, 0.01, 0.35, 0.01, 0.01, 0.01,
        ],
        2.0,
    );
    let mut harness = open_in_editor(&dir, &file);
    // Twelve channel lanes leave the meter strip no room at 720 px.
    harness.set_size(egui::vec2(1600.0, 1200.0));
    settle_meters(&mut harness, 1.0);
    let (view, vector) = harness
        .state()
        .test_mini_meter_channel_view()
        .expect("a mini meter");
    assert_eq!(view, "SURROUND");
    let [x, y, z] = vector.expect("sound");
    assert!(
        x < -0.3 && y > 0.4 && z > 0.1,
        "front left and up: {:?}",
        [x, y, z]
    );
    let out_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("debug")
        .join("screenshot_verify")
        .join("multichannel");
    std::fs::create_dir_all(&out_dir).expect("create evidence dir");
    harness
        .render()
        .expect("render")
        .save(out_dir.join("surround_714.png"))
        .expect("save the screenshot");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Evidence for the headphone UI: the top bar's toggle lit, the SURROUND
/// panel's HRTF mark, and the virtual speakers window on a 7.1.4 file with a
/// height speaker picked for the side view.
#[cfg(feature = "kittest_render")]
#[test]
fn kittest_render_the_virtual_speakers_window() {
    let dir = temp_dir("hrtf_render");
    let file = dir.join("immersive.wav");
    write_wav_with_mask(&file, 12, 0x2D63F, 2.0);
    let mut harness = open_in_editor(&dir, &file);
    harness.set_size(egui::vec2(1600.0, 1200.0));
    // Lay out at the new size first, or the click lands where the toggle was.
    harness.run_steps(3);
    harness.get_by_label("HRTF").click();
    wait_until(&mut harness, "the bundled HRTF", 60, |h| {
        h.state().test_binaural_channels() == Some(12)
    });
    assert!(harness.state_mut().test_request_hrtf_window());
    assert!(harness.state_mut().test_hrtf_select("TFL"));
    settle_meters(&mut harness, 1.0);
    let out_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("debug")
        .join("screenshot_verify")
        .join("multichannel");
    std::fs::create_dir_all(&out_dir).expect("create evidence dir");
    harness
        .render()
        .expect("render")
        .save(out_dir.join("hrtf_window_714.png"))
        .expect("save the screenshot");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 12 channels for 25 s: about 200 MB of spectrogram at the default
/// settings, more than the spectrogram cache budget on any machine (at
/// most 130 MB). It used to be evicted the moment it completed, and the
/// tab computed it again, and again, without ever drawing it.
#[test]
fn a_spectrogram_bigger_than_the_cache_completes_once_and_stays() {
    let dir = temp_dir("spectro_714");
    let file = dir.join("immersive_714.wav");
    write_multichannel(&file, 12, 25.0);
    let mut harness = open_in_editor(&dir, &file);
    assert!(harness
        .state_mut()
        .test_set_view_mode(ViewMode::Spectrogram));
    wait_until(&mut harness, "the spectrogram", 120, |h| {
        h.state().test_spectro_state(&file).0
    });
    let (_, _, generation) = harness.state().test_spectro_state(&file);
    for _ in 0..60 {
        harness.run_steps(1);
        std::thread::sleep(Duration::from_millis(5));
    }
    let (cached, inflight, now) = harness.state().test_spectro_state(&file);
    assert!(cached && !inflight, "still shown, not being computed again");
    assert_eq!(now, generation, "computed once");
    let _ = std::fs::remove_dir_all(&dir);
}
