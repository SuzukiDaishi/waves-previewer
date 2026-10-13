//! The rebuilt Inspector's contract: opening a tool changes nothing that is
//! heard; a changed setting is previewed by itself; Apply writes exactly the
//! samples the preview played; Ctrl+Z walks the adjustments back before it
//! undoes committed edits; Esc throws the adjustments away; one marker or
//! loop gesture is one undo step; and the unsaved mark follows the audio
//! through saves, undos and Revert to Saved. See `docs/INSPECTOR_SPEC.md`.
#![cfg(feature = "kittest")]

use std::path::PathBuf;
use std::time::{Duration, Instant};

use egui::{Key, Modifiers};
use egui_kittest::Harness;
use neowaves::app::ToolKind;
use neowaves::kittest::harness_with_startup;
use neowaves::{StartupConfig, WavesPreviewer};

type App = Harness<'static, WavesPreviewer>;

const FIXTURE_SR: u32 = 48_000;

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("neowaves_inspector_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn tone(freq: f32, amp: f32, secs: f32) -> Vec<f32> {
    (0..(FIXTURE_SR as f32 * secs) as usize)
        .map(|i| (i as f32 / FIXTURE_SR as f32 * freq * std::f32::consts::TAU).sin() * amp)
        .collect()
}

fn wait_until(harness: &mut App, what: &str, mut done: impl FnMut(&App) -> bool) {
    let start = Instant::now();
    loop {
        harness.run_steps(1);
        if done(harness) {
            return;
        }
        if start.elapsed() > Duration::from_secs(20) {
            panic!("timeout waiting for {what}");
        }
        std::thread::sleep(Duration::from_millis(15));
    }
}

/// A stereo file open in the editor.
fn open_editor(tag: &str) -> (App, PathBuf) {
    let dir = temp_dir(tag);
    let path = dir.join("tone.wav");
    neowaves::wave::export_channels_audio(
        &[tone(220.0, 0.3, 1.0), tone(330.0, 0.25, 1.0)],
        FIXTURE_SR,
        &path,
    )
    .expect("export fixture");
    let mut cfg = StartupConfig::default();
    cfg.open_folder = Some(dir.clone());
    cfg.open_first = false;
    let mut harness = harness_with_startup(cfg);
    wait_until(&mut harness, "scan", |h| !h.state().files.is_empty());
    assert!(harness.state_mut().test_open_tab_for_path(&path));
    wait_until(&mut harness, "tab ready", |h| {
        h.state()
            .active_tab
            .and_then(|idx| h.state().tabs.get(idx))
            .is_some_and(|tab| tab.samples_len > 0 && !tab.loading)
    });
    harness.run_steps(3);
    (harness, path)
}

fn wait_for_preview(harness: &mut App) {
    wait_until(harness, "preview", |h| {
        h.state().test_tool_preview_channels().is_some()
            && !h.state().test_preview_busy_for_active_tab()
    });
}

fn peak(channels: &[Vec<f32>]) -> f32 {
    channels
        .iter()
        .flat_map(|c| c.iter())
        .fold(0.0f32, |p, v| p.max(v.abs()))
}

#[test]
fn opening_a_tool_changes_nothing_until_a_setting_does() {
    let (mut harness, _) = open_editor("open");
    assert!(harness.state_mut().test_switch_tool(ToolKind::Gain));
    harness.run_steps(10);
    assert!(!harness.state().test_tool_session_previewing());
    assert!(harness.state().test_tool_preview_channels().is_none());
    assert!(!harness.state().test_preview_busy_for_active_tab());

    // A change starts the preview, without a Preview button.
    assert!(harness
        .state_mut()
        .test_change_tool_setting(|st| st.gain_db = 6.0));
    assert!(harness.state().test_tool_session_previewing());
    wait_for_preview(&mut harness);
    let original = harness.state().test_tab_channel_samples().unwrap();
    let preview = harness.state().test_tool_preview_channels().unwrap();
    let g = 10.0f32.powf(6.0 / 20.0);
    assert!((peak(&preview) - peak(&original) * g).abs() < 1e-3);
    assert_eq!(preview.len(), 2, "every channel is previewed");
}

#[test]
fn apply_writes_exactly_what_the_preview_played() {
    let (mut harness, _) = open_editor("apply");
    assert!(harness.state_mut().test_switch_tool(ToolKind::TimeStretch));
    assert!(harness
        .state_mut()
        .test_change_tool_setting(|st| st.stretch_rate = 1.3));
    wait_for_preview(&mut harness);
    let preview = harness.state().test_tool_preview_channels().unwrap();
    assert!(harness.state_mut().test_inspector_apply());
    wait_until(&mut harness, "apply", |h| {
        !h.state().test_editor_apply_busy()
    });
    let after = harness.state().test_tab_channel_samples().unwrap();
    assert_eq!(
        after, preview,
        "the committed samples are the previewed ones"
    );
    assert!(harness.state().test_tab_dirty());
    // Applied: the settings stay, nothing is previewed until they change.
    assert!(!harness.state().test_tool_session_previewing());
    assert!((harness.state().test_tool_state().unwrap().stretch_rate - 1.3).abs() < 1e-6);
}

#[test]
fn ctrl_z_walks_the_adjustments_back_then_undoes_the_edit() {
    let (mut harness, _) = open_editor("draft_undo");
    assert!(harness.state_mut().test_switch_tool(ToolKind::Gain));
    assert!(harness
        .state_mut()
        .test_change_tool_setting(|st| st.gain_db = 3.0));
    assert!(harness
        .state_mut()
        .test_change_tool_setting(|st| st.gain_db = 9.0));
    assert_eq!(harness.state().test_tool_draft_depths(), (2, 0));

    harness.key_press_modifiers(Modifiers::COMMAND, Key::Z);
    harness.run_steps(2);
    assert_eq!(harness.state().test_tool_state().unwrap().gain_db, 3.0);
    harness.key_press_modifiers(Modifiers::COMMAND, Key::Y);
    harness.run_steps(2);
    assert_eq!(harness.state().test_tool_state().unwrap().gain_db, 9.0);
    harness.key_press_modifiers(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z);
    harness.run_steps(2);
    assert_eq!(
        harness.state().test_tool_state().unwrap().gain_db,
        9.0,
        "nothing left to redo in the draft, and no edit to redo either"
    );

    // Apply, then Ctrl+Z undoes the apply itself.
    assert!(harness.state_mut().test_inspector_apply());
    harness.run_steps(2);
    assert!(harness.state().test_tab_dirty());
    assert_eq!(harness.state().test_tool_draft_depths(), (0, 0));
    harness.key_press_modifiers(Modifiers::COMMAND, Key::Z);
    harness.run_steps(2);
    assert!(
        !harness.state().test_tab_dirty(),
        "the gain apply is undone"
    );
}

#[test]
fn escape_discards_the_adjustments() {
    let (mut harness, _) = open_editor("escape");
    assert!(harness.state_mut().test_switch_tool(ToolKind::Gain));
    assert!(harness
        .state_mut()
        .test_change_tool_setting(|st| st.gain_db = 5.0));
    wait_for_preview(&mut harness);
    harness.key_press(Key::Escape);
    harness.run_steps(2);
    assert_eq!(harness.state().test_tool_state().unwrap().gain_db, 0.0);
    assert!(harness.state().test_tool_preview_channels().is_none());
    assert!(!harness.state().test_tool_session_previewing());
    assert!(!harness.state().test_tab_dirty());
}

#[test]
fn ctrl_enter_applies() {
    let (mut harness, _) = open_editor("ctrl_enter");
    assert!(harness.state_mut().test_switch_tool(ToolKind::Normalize));
    assert!(harness
        .state_mut()
        .test_change_tool_setting(|st| st.normalize_target_db = -3.0));
    harness.key_press_modifiers(Modifiers::COMMAND, Key::Enter);
    harness.run_steps(3);
    assert!(harness.state().test_tab_dirty());
    let target = 10.0f32.powf(-3.0 / 20.0);
    assert!((peak(&harness.state().test_tab_channel_samples().unwrap()) - target).abs() < 1e-3);
}

#[test]
fn neutral_settings_cannot_be_applied() {
    let (mut harness, _) = open_editor("neutral");
    assert!(harness.state_mut().test_switch_tool(ToolKind::Gain));
    assert!(!harness.state_mut().test_inspector_apply());
    assert!(harness.state().test_editor_undo_labels().is_empty());
}

#[test]
fn switching_tools_keeps_playback_running() {
    let (mut harness, _) = open_editor("switch_play");
    assert!(harness.state_mut().test_switch_tool(ToolKind::Gain));
    assert!(harness
        .state_mut()
        .test_change_tool_setting(|st| st.gain_db = -6.0));
    wait_for_preview(&mut harness);
    harness.state_mut().test_request_workspace_play_toggle();
    wait_until(&mut harness, "playing", |h| {
        h.state().test_audio_is_playing()
    });
    assert!(harness.state_mut().test_switch_tool(ToolKind::Reverse));
    harness.run_steps(2);
    assert!(
        harness.state().test_audio_is_playing(),
        "a tool switch must not stop playback"
    );
    assert!(harness.state().test_tool_preview_channels().is_none());
}

#[test]
fn a_loop_gesture_is_one_undo_step() {
    let (mut harness, _) = open_editor("loop_gesture");
    let before = harness.state().test_editor_undo_labels().len();
    assert!(harness.state_mut().test_annotation_gesture(
        "Loop Edge",
        &[
            Some((1_000, 20_000)),
            Some((1_200, 20_000)),
            Some((1_400, 20_000))
        ],
    ));
    let labels = harness.state().test_editor_undo_labels();
    assert_eq!(labels.len(), before + 1);
    assert_eq!(labels.last().map(String::as_str), Some("Loop Edge"));
    // A gesture that ends where it began leaves nothing to undo.
    assert!(harness
        .state_mut()
        .test_annotation_gesture("Loop Edge", &[Some((1_400, 20_000))]));
    assert_eq!(harness.state().test_editor_undo_labels().len(), before + 1);
}

#[test]
fn an_undo_after_a_save_is_unsaved_again() {
    let (mut harness, _) = open_editor("undo_after_save");
    assert!(harness.state_mut().test_switch_tool(ToolKind::Gain));
    assert!(harness
        .state_mut()
        .test_change_tool_setting(|st| st.gain_db = 4.0));
    assert!(harness.state_mut().test_inspector_apply());
    harness.run_steps(2);
    assert!(harness.state_mut().test_mark_active_tab_saved());
    assert!(!harness.state().test_tab_dirty());
    harness.key_press_modifiers(Modifiers::COMMAND, Key::Z);
    harness.run_steps(2);
    assert!(
        harness.state().test_tab_dirty(),
        "the audio no longer matches the saved file"
    );
    harness.key_press_modifiers(Modifiers::COMMAND, Key::Y);
    harness.run_steps(2);
    assert!(
        !harness.state().test_tab_dirty(),
        "redo brings back the saved audio"
    );
}

#[test]
fn revert_to_saved_can_be_undone() {
    let (mut harness, _) = open_editor("revert");
    let original = harness.state().test_tab_channel_samples().unwrap();
    assert!(harness.state_mut().test_switch_tool(ToolKind::Gain));
    assert!(harness
        .state_mut()
        .test_change_tool_setting(|st| st.gain_db = -9.0));
    assert!(harness.state_mut().test_inspector_apply());
    harness.run_steps(2);
    let edited = harness.state().test_tab_channel_samples().unwrap();
    assert!(harness.state_mut().test_revert_active_tab_to_saved());
    wait_until(&mut harness, "revert", |h| !h.state().test_revert_pending());
    assert!(!harness.state().test_tab_dirty());
    assert_eq!(
        harness.state().test_tab_channel_samples().unwrap(),
        original
    );
    assert_eq!(
        harness
            .state()
            .test_editor_undo_labels()
            .last()
            .map(String::as_str),
        Some("Revert to Saved")
    );
    harness.key_press_modifiers(Modifiers::COMMAND, Key::Z);
    harness.run_steps(2);
    assert!(harness.state().test_tab_dirty());
    assert_eq!(harness.state().test_tab_channel_samples().unwrap(), edited);
}

fn rms(samples: &[f32]) -> f32 {
    (samples.iter().map(|v| v * v).sum::<f32>() / samples.len().max(1) as f32).sqrt()
}

#[test]
fn spectral_mute_and_heal_work_on_the_selection() {
    let (mut harness, _) = open_editor("mute_heal");
    let len = harness.state().test_tab_samples_len();
    let middle = (len * 9 / 20)..(len * 11 / 20);
    let outside = 0..(len / 10);

    // Both need a selection: without one there is nothing to apply.
    assert!(harness.state_mut().test_switch_tool(ToolKind::SpectralMute));
    harness.run_steps(2);
    assert!(!harness.state_mut().test_inspector_apply());

    assert!(harness.state_mut().test_set_selection_frac(0.4, 0.6));
    let before = harness.state().test_tab_channel_samples().unwrap();
    let (undo_before, _) = harness.state().test_editor_history_depths();
    assert!(harness.state_mut().test_inspector_apply());
    wait_until(&mut harness, "mute", |h| {
        !h.state().test_editor_apply_busy()
    });
    let muted = harness.state().test_tab_channel_samples().unwrap();
    for (ch, (m, b)) in muted.iter().zip(&before).enumerate() {
        assert!(
            rms(&m[middle.clone()]) < 1e-3,
            "channel {ch} is silent inside"
        );
        assert_eq!(
            m[outside.clone()],
            b[outside.clone()],
            "channel {ch} is untouched outside"
        );
    }
    assert_eq!(
        harness.state().test_editor_history_depths().0,
        undo_before + 1
    );
    assert_eq!(
        harness
            .state()
            .test_editor_undo_labels()
            .last()
            .map(String::as_str),
        Some("Spectral Mute")
    );

    // Heal rebuilds the silenced stretch from the tone around it.
    assert!(harness.state_mut().test_switch_tool(ToolKind::Heal));
    harness.run_steps(2);
    assert!(harness.state_mut().test_set_selection_frac(0.4, 0.6));
    assert!(harness.state_mut().test_inspector_apply());
    wait_until(&mut harness, "heal", |h| {
        !h.state().test_editor_apply_busy()
    });
    let healed = harness.state().test_tab_channel_samples().unwrap();
    for (ch, h) in healed.iter().enumerate() {
        assert!(
            rms(&h[middle.clone()]) > 0.05,
            "channel {ch} is rebuilt inside"
        );
    }
    assert_eq!(
        harness.state().test_editor_history_depths().0,
        undo_before + 2
    );
}

#[test]
fn a_brush_apply_spends_its_strokes() {
    let (mut harness, _) = open_editor("brush_spend");
    assert!(harness
        .state_mut()
        .test_set_view_mode(neowaves::ViewMode::Spectrogram));
    assert!(harness
        .state_mut()
        .test_switch_tool(ToolKind::SpectralBrush));
    harness.run_steps(2);
    assert!(harness
        .state_mut()
        .test_spectral_brush_stamp(0.5, 220.0, 30.0));
    assert_eq!(harness.state().test_spectral_brush_stamp_count(), 1);

    assert!(harness.state_mut().test_inspector_apply());
    wait_until(&mut harness, "brush", |h| {
        !h.state().test_editor_apply_busy()
    });
    assert_eq!(
        harness.state().test_spectral_brush_stamp_count(),
        0,
        "the strokes are in the audio now"
    );
    // Neither Esc nor a second Apply brings them back to cut twice.
    harness.state_mut().test_inspector_discard();
    assert_eq!(harness.state().test_spectral_brush_stamp_count(), 0);
    assert!(!harness.state_mut().test_inspector_apply());
}

#[test]
fn shown_music_markers_are_not_committed_by_another_marker_edit() {
    let (mut harness, _) = open_editor("music_markers");
    let len = harness.state().test_tab_samples_len();
    assert!(harness.state_mut().test_switch_tool(ToolKind::MusicAnalyze));
    assert!(harness.state_mut().test_set_music_analysis_result_data(
        vec![len / 4, len / 2, len * 3 / 4],
        vec![len / 4],
        vec![],
        len,
    ));
    assert!(harness.state_mut().test_show_music_markers() > 0);
    let tab_idx = harness.state().active_tab.unwrap();
    assert!(
        harness.state().tabs[tab_idx].markers.is_empty(),
        "shown beats are drawn over the waveform, not added"
    );

    // A marker added meanwhile is committed alone.
    harness.state_mut().test_audio_seek_to_sample(len / 8);
    harness.key_press(Key::M);
    harness.run_steps(3);
    let tab = &harness.state().tabs[tab_idx];
    assert_eq!(tab.markers.len(), 1);
    assert!(tab
        .markers_committed
        .iter()
        .all(|m| !m.label.starts_with("AI_")));

    // Add as markers: one undo step, and the overlay goes.
    let (undo_before, _) = harness.state().test_editor_history_depths();
    assert!(harness.state_mut().test_add_music_markers());
    harness.run_steps(2);
    let tab = &harness.state().tabs[tab_idx];
    assert!(tab.markers.len() > 1);
    assert!(tab.music_analysis_draft.provisional_markers.is_empty());
    assert_eq!(
        harness.state().test_editor_history_depths().0,
        undo_before + 1
    );
}

#[test]
fn a_saved_preview_is_rendered_again_when_the_session_opens() {
    let (mut harness, path) = open_editor("session_preview");
    assert!(harness.state_mut().test_switch_tool(ToolKind::Gain));
    assert!(harness
        .state_mut()
        .test_change_tool_setting(|st| st.gain_db = 6.0));
    wait_for_preview(&mut harness);
    let preview = harness.state().test_tool_preview_channels().unwrap();

    let session = path.with_extension("nwsess");
    assert!(harness.state_mut().test_save_session_to(&session));
    assert!(harness.state_mut().test_open_session_from(&session));
    // The saved picture is shown at once; the engine renders the same
    // settings again, which is what Apply will commit.
    wait_until(&mut harness, "restored preview", |h| {
        h.state().test_tool_preview_channels().is_some()
            && !h.state().test_preview_busy_for_active_tab()
    });
    assert_eq!(
        harness.state().test_tool_preview_channels().unwrap(),
        preview
    );
    assert!(harness.state_mut().test_inspector_apply());
    wait_until(&mut harness, "apply", |h| {
        !h.state().test_editor_apply_busy()
    });
    assert_eq!(harness.state().test_tab_channel_samples().unwrap(), preview);
}
