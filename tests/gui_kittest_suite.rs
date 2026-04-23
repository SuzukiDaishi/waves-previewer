#[cfg(feature = "kittest")]
mod kittest_suite {
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::OnceLock;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use egui::{Key, Modifiers, MouseWheelUnit};
    use egui_kittest::{
        kittest::{NodeT, Queryable},
        Harness,
    };
    use neowaves::app::ToolKind;
    use neowaves::kittest::{harness_default, harness_with_startup};
    use neowaves::{StartupConfig, WavesPreviewer};
    use walkdir::WalkDir;

    const DEFAULT_WAV_DIR: &str = "test_samples";
    const SCAN_TIMEOUT: Duration = Duration::from_secs(30);
    const TAB_READY_TIMEOUT: Duration = Duration::from_secs(30);

    fn source_wav_dir() -> PathBuf {
        let from_env = std::env::var("WAVES_PREVIEWER_TEST_WAV_DIR").ok();
        let path = from_env
            .map(PathBuf::from)
            .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join(DEFAULT_WAV_DIR));
        assert!(path.is_dir(), "test wav dir not found: {}", path.display());
        path
    }

    fn wav_dir() -> PathBuf {
        static FIXTURE_DIR: OnceLock<PathBuf> = OnceLock::new();
        FIXTURE_DIR
            .get_or_init(|| {
                let src = source_wav_dir();
                let dst = make_temp_dir("kittest_media");
                for entry in WalkDir::new(&src).follow_links(false) {
                    let entry = match entry {
                        Ok(entry) => entry,
                        Err(_) => continue,
                    };
                    if !entry.file_type().is_file() {
                        continue;
                    }
                    let Ok(rel) = entry.path().strip_prefix(&src) else {
                        continue;
                    };
                    let out = dst.join(rel);
                    if let Some(parent) = out.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    let _ = std::fs::copy(entry.path(), out);
                }
                maybe_generate_extra_formats(&dst);
                dst
            })
            .clone()
    }

    fn has_file_ext(dir: &Path, ext: &str) -> bool {
        for entry in WalkDir::new(dir).follow_links(false) {
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => continue,
            };
            if !entry.file_type().is_file() {
                continue;
            }
            let matches = entry
                .path()
                .extension()
                .and_then(|s| s.to_str())
                .map(|s| s.eq_ignore_ascii_case(ext))
                .unwrap_or(false);
            if matches {
                return true;
            }
        }
        false
    }

    fn first_wav_file(dir: &Path) -> Option<PathBuf> {
        for entry in WalkDir::new(dir).follow_links(false) {
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => continue,
            };
            if !entry.file_type().is_file() {
                continue;
            }
            let is_wav = entry
                .path()
                .extension()
                .and_then(|s| s.to_str())
                .map(|s| s.eq_ignore_ascii_case("wav"))
                .unwrap_or(false);
            if is_wav {
                return Some(entry.into_path());
            }
        }
        None
    }

    fn first_n_audio_files(dir: &Path, count: usize) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for entry in WalkDir::new(dir).follow_links(false) {
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => continue,
            };
            if !entry.file_type().is_file() {
                continue;
            }
            let supported = entry
                .path()
                .extension()
                .and_then(|s| s.to_str())
                .map(neowaves::audio_io::is_supported_extension)
                .unwrap_or(false);
            if supported {
                out.push(entry.into_path());
                if out.len() >= count {
                    break;
                }
            }
        }
        out
    }

    fn try_ffmpeg_convert(src: &Path, dst: &Path) -> bool {
        Command::new("ffmpeg")
            .arg("-y")
            .arg("-hide_banner")
            .arg("-loglevel")
            .arg("error")
            .arg("-i")
            .arg(src)
            .arg(dst)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    fn try_sox_convert(src: &Path, dst: &Path) -> bool {
        Command::new("sox")
            .arg(src)
            .arg(dst)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    fn try_internal_convert(src: &Path, dst: &Path) -> bool {
        match neowaves::audio_io::decode_audio_multi(src) {
            Ok((chans, sr)) => neowaves::wave::export_channels_audio(&chans, sr, dst).is_ok(),
            Err(_) => false,
        }
    }

    fn maybe_generate_extra_formats(dir: &Path) {
        let Some(seed) = first_wav_file(dir) else {
            return;
        };
        for ext in ["mp3", "m4a", "ogg"] {
            if has_file_ext(dir, ext) {
                continue;
            }
            let out = dir.join(format!("generated_fixture.{ext}"));
            let ok = try_ffmpeg_convert(&seed, &out)
                || ((ext == "mp3" || ext == "ogg") && try_sox_convert(&seed, &out))
                || try_internal_convert(&seed, &out);
            if !ok {
                eprintln!(
                    "warning: could not generate {} fixture from {}",
                    ext,
                    seed.display()
                );
            }
        }
    }

    fn harness_with_wavs(open_first: bool) -> Harness<'static, WavesPreviewer> {
        let mut cfg = StartupConfig::default();
        cfg.open_folder = Some(wav_dir());
        cfg.open_first = open_first;
        harness_with_startup(cfg)
    }

    fn make_temp_dir(tag: &str) -> PathBuf {
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let seq = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "neowaves_kittest_{tag}_{}_{}_{}",
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

    fn synth_dynamic_stereo(sr: u32, secs: f32) -> Vec<Vec<f32>> {
        let frames = ((sr as f32) * secs).max(1.0) as usize;
        let mut left = Vec::with_capacity(frames);
        let mut right = Vec::with_capacity(frames);
        for i in 0..frames {
            let t = (i as f32) / (sr as f32);
            let phase = (t / secs.max(0.001)).clamp(0.0, 1.0);
            let envelope: f32 = if phase < 0.20 {
                0.08
            } else if phase < 0.45 {
                0.75
            } else if phase < 0.70 {
                0.25
            } else {
                0.55
            };
            let pulse: f32 = if (t * 7.0).fract() < 0.12 { 0.35 } else { 0.0 };
            left.push((t * 180.0 * std::f32::consts::TAU).sin() * (envelope + pulse).min(0.95));
            right.push((t * 360.0 * std::f32::consts::TAU).sin() * envelope.min(0.85));
        }
        vec![left, right]
    }

    fn build_format_fixtures(dir: &Path, secs: f32) -> Vec<PathBuf> {
        let sr = 44_100;
        let chans = synth_stereo(sr, secs);
        let mut out = Vec::new();
        for ext in ["wav", "mp3", "m4a", "ogg"] {
            let path = dir.join(format!("fixture_{ext}.{ext}"));
            neowaves::wave::export_channels_audio(&chans, sr, &path)
                .unwrap_or_else(|e| panic!("export {ext} failed: {e}"));
            out.push(path);
        }
        out
    }

    fn harness_with_folder(dir: PathBuf) -> Harness<'static, WavesPreviewer> {
        let mut cfg = StartupConfig::default();
        cfg.open_folder = Some(dir);
        cfg.open_first = false;
        harness_with_startup(cfg)
    }

    fn harness_with_editor_fixture() -> Harness<'static, WavesPreviewer> {
        let dir = make_temp_dir("editor_fixture");
        let sr = 48_000;
        let chans = synth_stereo(sr, 3.0);
        let path = dir.join("editor_fixture.wav");
        neowaves::wave::export_channels_audio(&chans, sr, &path)
            .unwrap_or_else(|e| panic!("export editor fixture failed: {e}"));
        harness_with_folder(dir)
    }

    fn harness_with_dynamic_editor_fixture() -> Harness<'static, WavesPreviewer> {
        let dir = make_temp_dir("dynamic_editor_fixture");
        let sr = 48_000;
        let chans = synth_dynamic_stereo(sr, 6.0);
        let path = dir.join("dynamic_editor_fixture.wav");
        neowaves::wave::export_channels_audio(&chans, sr, &path)
            .unwrap_or_else(|e| panic!("export dynamic editor fixture failed: {e}"));
        harness_with_folder(dir)
    }

    fn audio_buffer_len(state: &WavesPreviewer) -> usize {
        state
            .audio
            .shared
            .samples
            .load()
            .as_ref()
            .map(|b| b.len())
            .unwrap_or(0)
    }

    fn harness_empty() -> Harness<'static, WavesPreviewer> {
        harness_default()
    }

    fn sample_wav_files(count: usize) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for entry in WalkDir::new(wav_dir()).follow_links(false) {
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => continue,
            };
            if entry.file_type().is_file() {
                let path = entry.path();
                let is_wav = path
                    .extension()
                    .and_then(|s| s.to_str())
                    .map(|s| s.eq_ignore_ascii_case("wav"))
                    .unwrap_or(false);
                if is_wav {
                    out.push(path.to_path_buf());
                    if out.len() >= count {
                        break;
                    }
                }
            }
        }
        assert!(out.len() >= count, "not enough wavs");
        out
    }

    fn wait_for_scan(harness: &mut Harness<'static, WavesPreviewer>) {
        let start = Instant::now();
        loop {
            harness.run_steps(1);
            let (done, has_files) = {
                let state = harness.state();
                (!state.scan_in_progress, !state.files.is_empty())
            };
            // Most UI tests only need the list to become usable.
            if (done && has_files) || (has_files && start.elapsed() > Duration::from_secs(5)) {
                break;
            }
            if start.elapsed() > SCAN_TIMEOUT {
                panic!("scan timeout");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn wait_for_tab(harness: &mut Harness<'static, WavesPreviewer>) {
        let start = Instant::now();
        loop {
            harness.run_steps(1);
            if harness.state().active_tab.is_some() {
                break;
            }
            if start.elapsed() > Duration::from_secs(10) {
                panic!("tab open timeout");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn wait_for_tab_ready(harness: &mut Harness<'static, WavesPreviewer>) {
        let start = Instant::now();
        loop {
            harness.run_steps(1);
            if let Some(idx) = harness.state().active_tab {
                if let Some(tab) = harness.state().tabs.get(idx) {
                    if tab.samples_len > 0
                        && (!tab.loading || harness.state().test_audio_has_samples())
                    {
                        break;
                    }
                }
            }
            if start.elapsed() > TAB_READY_TIMEOUT {
                panic!("tab decode timeout");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn wait_for_editor_apply(harness: &mut Harness<'static, WavesPreviewer>) {
        let start = Instant::now();
        loop {
            harness.run_steps(1);
            if !harness.state().test_editor_apply_active() {
                break;
            }
            if start.elapsed() > Duration::from_secs(20) {
                panic!("editor apply timeout");
            }
            std::thread::sleep(Duration::from_millis(30));
        }
    }

    fn wait_for_virtual_trim_done(harness: &mut Harness<'static, WavesPreviewer>) {
        let start = Instant::now();
        loop {
            harness.run_steps(1);
            if !harness.state().test_virtual_trim_active() {
                break;
            }
            if start.elapsed() > Duration::from_secs(20) {
                panic!(
                    "virtual trim timeout progress={:?}",
                    harness.state().test_virtual_trim_progress()
                );
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn wait_for_preview_tool(
        harness: &mut Harness<'static, WavesPreviewer>,
        tool: ToolKind,
        require_overlay: bool,
    ) {
        let start = Instant::now();
        loop {
            harness.run_steps(1);
            let tool_ok = harness.state().test_preview_audio_tool() == Some(tool);
            let overlay_ok =
                !require_overlay || harness.state().test_preview_overlay_tool() == Some(tool);
            if tool_ok && overlay_ok {
                break;
            }
            if start.elapsed() > Duration::from_secs(10) {
                panic!(
                    "preview timeout for {:?}: audio={:?} overlay={:?}",
                    tool,
                    harness.state().test_preview_audio_tool(),
                    harness.state().test_preview_overlay_tool()
                );
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn wait_for_preview_idle(harness: &mut Harness<'static, WavesPreviewer>) {
        let start = Instant::now();
        loop {
            harness.run_steps(1);
            if !harness.state().test_preview_busy_for_active_tab() {
                break;
            }
            if start.elapsed() > Duration::from_secs(10) {
                panic!("preview idle timeout");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn ensure_editor_ready(harness: &mut Harness<'static, WavesPreviewer>) {
        if harness.state().active_tab.is_none() {
            assert!(harness.state_mut().test_open_first_tab());
            wait_for_tab(harness);
        }
        wait_for_tab_ready(harness);
    }

    fn path_for_row(state: &WavesPreviewer, row: usize) -> PathBuf {
        let id = state.files[row];
        let idx = *state.item_index.get(&id).expect("missing item id");
        state.items[idx].path.clone()
    }

    fn select_first_row(harness: &mut Harness<'static, WavesPreviewer>) -> PathBuf {
        let path = {
            let state = harness.state();
            path_for_row(state, 0)
        };
        let label = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        harness.get_by_label(label).click();
        harness.run_steps(2);
        assert_eq!(harness.state().test_playing_path(), Some(&path));
        path
    }

    fn open_first_tab(harness: &mut Harness<'static, WavesPreviewer>) -> PathBuf {
        let path = select_first_row(harness);
        harness.key_press(Key::Enter);
        wait_for_tab(harness);
        path
    }

    const EDITOR_AMPLITUDE_NAV_GAP: f32 = 6.0;
    const EDITOR_AMPLITUDE_NAV_RIGHT_PAD: f32 = 6.0;
    const EDITOR_AMPLITUDE_NAV_STRIP_W: f32 = 18.0;
    const EDITOR_AMPLITUDE_NAV_RESERVED_W: f32 =
        EDITOR_AMPLITUDE_NAV_GAP + EDITOR_AMPLITUDE_NAV_RIGHT_PAD + EDITOR_AMPLITUDE_NAV_STRIP_W;

    fn editor_canvas_side_label<'a>(
        harness: &'a Harness<'static, WavesPreviewer>,
        label: &'a str,
    ) -> egui_kittest::Node<'a> {
        let inspector_rect = harness.get_by_label("Inspector").rect();
        harness
            .query_all_by_label(label)
            .filter(|node| node.rect().right() < inspector_rect.left())
            .min_by(|a, b| {
                a.rect()
                    .min
                    .y
                    .partial_cmp(&b.rect().min.y)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .unwrap_or_else(|| panic!("Canvas-side label '{label}' not found"))
    }

    fn editor_canvas_hover_pos(harness: &Harness<'static, WavesPreviewer>) -> egui::Pos2 {
        let inspector_rect = harness.get_by_label("Inspector").rect();
        egui::pos2(
            (inspector_rect.left() - EDITOR_AMPLITUDE_NAV_RESERVED_W - 220.0).max(40.0),
            inspector_rect.center().y,
        )
    }

    fn editor_zoom_in_once(harness: &mut Harness<'static, WavesPreviewer>) {
        let hover_pos = editor_canvas_hover_pos(harness);
        harness.hover_at(hover_pos);
        harness.event_modifiers(
            egui::Event::MouseWheel {
                unit: MouseWheelUnit::Point,
                delta: egui::vec2(0.0, 120.0),
                modifiers: Modifiers::COMMAND,
            },
            Modifiers::COMMAND,
        );
        harness.run_steps(3);
    }

    fn editor_zoom_out_once(harness: &mut Harness<'static, WavesPreviewer>) {
        let hover_pos = editor_canvas_hover_pos(harness);
        harness.hover_at(hover_pos);
        harness.event_modifiers(
            egui::Event::MouseWheel {
                unit: MouseWheelUnit::Point,
                delta: egui::vec2(0.0, -120.0),
                modifiers: Modifiers::COMMAND,
            },
            Modifiers::COMMAND,
        );
        harness.run_steps(3);
    }

    fn editor_shift_pan_once(harness: &mut Harness<'static, WavesPreviewer>) {
        let hover_pos = editor_canvas_hover_pos(harness);
        harness.hover_at(hover_pos);
        harness.event_modifiers(
            egui::Event::MouseWheel {
                unit: MouseWheelUnit::Point,
                delta: egui::vec2(0.0, 120.0),
                modifiers: Modifiers::SHIFT,
            },
            Modifiers::SHIFT,
        );
        harness.run_steps(3);
    }

    fn editor_horizontal_pan_once(harness: &mut Harness<'static, WavesPreviewer>, delta_x: f32) {
        let hover_pos = editor_canvas_hover_pos(harness);
        harness.hover_at(hover_pos);
        harness.event_modifiers(
            egui::Event::MouseWheel {
                unit: MouseWheelUnit::Point,
                delta: egui::vec2(delta_x, 0.0),
                modifiers: Modifiers::NONE,
            },
            Modifiers::NONE,
        );
        harness.run_steps(3);
    }

    fn editor_plain_vertical_wheel_once(harness: &mut Harness<'static, WavesPreviewer>) {
        let hover_pos = editor_canvas_hover_pos(harness);
        harness.hover_at(hover_pos);
        harness.event_modifiers(
            egui::Event::MouseWheel {
                unit: MouseWheelUnit::Point,
                delta: egui::vec2(0.0, 120.0),
                modifiers: Modifiers::NONE,
            },
            Modifiers::NONE,
        );
        harness.run_steps(3);
    }

    fn editor_canvas_pos_at_frac(
        harness: &Harness<'static, WavesPreviewer>,
        frac: f32,
    ) -> egui::Pos2 {
        let wave_left = editor_wave_left(harness);
        let wave_w = editor_wave_width(harness).max(64.0);
        let inspector_rect = harness.get_by_label("Inspector").rect();
        egui::pos2(
            wave_left + (wave_w - 12.0) * frac.clamp(0.0, 1.0),
            inspector_rect.center().y,
        )
    }

    fn editor_wave_left(harness: &Harness<'static, WavesPreviewer>) -> f32 {
        if let Some(nav_rect) = harness.state().test_tab_amplitude_nav_rect() {
            return nav_rect.left()
                - editor_wave_width(harness).max(64.0)
                - EDITOR_AMPLITUDE_NAV_GAP;
        }
        let inspector_rect = harness.get_by_label("Inspector").rect();
        let wave_w = editor_wave_width(harness).max(64.0);
        let wave_right = (inspector_rect.left() - 4.0 - EDITOR_AMPLITUDE_NAV_RESERVED_W).max(48.0);
        (wave_right - wave_w + 8.0).max(8.0)
    }

    fn editor_canvas_pos_at_x_offset(
        harness: &Harness<'static, WavesPreviewer>,
        x_offset: f32,
    ) -> egui::Pos2 {
        let inspector_rect = harness.get_by_label("Inspector").rect();
        egui::pos2(
            editor_wave_left(harness) + x_offset,
            inspector_rect.center().y,
        )
    }

    fn editor_zoom_in_at_frac(harness: &mut Harness<'static, WavesPreviewer>, frac: f32) {
        let hover_pos = editor_canvas_pos_at_frac(harness, frac);
        harness.hover_at(hover_pos);
        harness.event_modifiers(
            egui::Event::MouseWheel {
                unit: MouseWheelUnit::Point,
                delta: egui::vec2(0.0, 120.0),
                modifiers: Modifiers::COMMAND,
            },
            Modifiers::COMMAND,
        );
        harness.run_steps(3);
    }

    fn editor_shift_click_at_frac(harness: &mut Harness<'static, WavesPreviewer>, frac: f32) {
        let pos = editor_canvas_pos_at_frac(harness, frac);
        harness.hover_at(pos);
        harness.event_modifiers(
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::SHIFT,
            },
            Modifiers::SHIFT,
        );
        harness.event_modifiers(
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: Modifiers::SHIFT,
            },
            Modifiers::SHIFT,
        );
        harness.run_steps(2);
    }

    fn editor_shift_right_drag(
        harness: &mut Harness<'static, WavesPreviewer>,
        start_frac: f32,
        end_frac: f32,
    ) {
        let start = editor_canvas_pos_at_frac(harness, start_frac);
        let end = editor_canvas_pos_at_frac(harness, end_frac);
        editor_shift_right_drag_between(harness, start, end);
    }

    fn editor_shift_right_drag_between(
        harness: &mut Harness<'static, WavesPreviewer>,
        start: egui::Pos2,
        end: egui::Pos2,
    ) {
        harness.hover_at(start);
        harness.run_steps(1);
        harness.event_modifiers(
            egui::Event::PointerButton {
                pos: start,
                button: egui::PointerButton::Secondary,
                pressed: true,
                modifiers: Modifiers::SHIFT,
            },
            Modifiers::SHIFT,
        );
        harness.run_steps(1);
        harness.event_modifiers(egui::Event::PointerMoved(end), Modifiers::SHIFT);
        harness.run_steps(2);
        harness.event_modifiers(
            egui::Event::PointerButton {
                pos: end,
                button: egui::PointerButton::Secondary,
                pressed: false,
                modifiers: Modifiers::SHIFT,
            },
            Modifiers::SHIFT,
        );
        harness.run_steps(2);
    }

    fn editor_shift_click_at_pos(harness: &mut Harness<'static, WavesPreviewer>, pos: egui::Pos2) {
        harness.hover_at(pos);
        harness.event_modifiers(
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::SHIFT,
            },
            Modifiers::SHIFT,
        );
        harness.event_modifiers(
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: Modifiers::SHIFT,
            },
            Modifiers::SHIFT,
        );
        harness.run_steps(2);
    }

    fn editor_small_middle_drag_pan(harness: &mut Harness<'static, WavesPreviewer>, dx: f32) {
        let start = editor_canvas_hover_pos(harness);
        let end = egui::pos2(start.x + dx, start.y);
        harness.hover_at(start);
        harness.event_modifiers(
            egui::Event::PointerButton {
                pos: start,
                button: egui::PointerButton::Middle,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
            Modifiers::NONE,
        );
        harness.event_modifiers(egui::Event::PointerMoved(end), Modifiers::NONE);
        harness.run_steps(1);
        harness.event_modifiers(
            egui::Event::PointerButton {
                pos: end,
                button: egui::PointerButton::Middle,
                pressed: false,
                modifiers: Modifiers::NONE,
            },
            Modifiers::NONE,
        );
        harness.run_steps(1);
    }

    fn editor_visible_samples(harness: &Harness<'static, WavesPreviewer>) -> usize {
        let tab_idx = harness.state().active_tab.expect("active tab");
        let tab = &harness.state().tabs[tab_idx];
        (tab.samples_per_px.max(0.0001) * editor_wave_width(harness)).ceil() as usize
    }

    fn editor_sample_at_ratio(harness: &Harness<'static, WavesPreviewer>, ratio: f32) -> usize {
        let tab_idx = harness.state().active_tab.expect("active tab");
        let tab = &harness.state().tabs[tab_idx];
        tab.view_offset
            .saturating_add(
                (editor_visible_samples(harness) as f32 * ratio.clamp(0.0, 1.0)) as usize,
            )
            .min(tab.samples_len)
    }

    fn editor_wave_width(harness: &Harness<'static, WavesPreviewer>) -> f32 {
        let tab_idx = harness.state().active_tab.expect("active tab");
        harness.state().tabs[tab_idx].last_wave_w.max(64.0)
    }

    fn editor_center_display_sample(harness: &Harness<'static, WavesPreviewer>) -> usize {
        let (start, end) = harness
            .state()
            .test_editor_visible_display_range()
            .expect("visible display range");
        start + end.saturating_sub(start) / 2
    }

    fn assert_editor_whole_fit(harness: &Harness<'static, WavesPreviewer>, label: &str) {
        let tab_idx = harness.state().active_tab.expect("active tab");
        let tab = &harness.state().tabs[tab_idx];
        let display_len = harness
            .state()
            .test_editor_display_samples_len()
            .expect("display length");
        let wave_w = editor_wave_width(harness);
        let expected_spp = (display_len as f32 / wave_w.max(1.0)).max(0.0025);
        let tolerance = expected_spp.max(1.0) * 0.01;
        assert!(
            (tab.samples_per_px - expected_spp).abs() <= tolerance,
            "{label}: samples_per_px should fit whole file: actual={} expected={} tolerance={}",
            tab.samples_per_px,
            expected_spp,
            tolerance
        );
        assert_eq!(tab.view_offset, 0, "{label}: view_offset should be 0");
        assert!(
            tab.view_offset_exact.abs() <= 0.5,
            "{label}: view_offset_exact should be near 0, got {}",
            tab.view_offset_exact
        );
        let (start, end) = harness
            .state()
            .test_editor_visible_display_range()
            .expect("visible display range");
        assert_eq!(start, 0, "{label}: visible start should be 0");
        assert_eq!(
            end, display_len,
            "{label}: visible end should reach display length"
        );
    }

    fn editor_amplitude_nav_rect(harness: &Harness<'static, WavesPreviewer>) -> egui::Rect {
        harness
            .state()
            .test_tab_amplitude_nav_rect()
            .expect("amplitude nav rect")
    }

    fn editor_amplitude_nav_viewport_rect(
        harness: &Harness<'static, WavesPreviewer>,
    ) -> egui::Rect {
        harness
            .state()
            .test_tab_amplitude_nav_viewport_rect()
            .expect("amplitude nav viewport rect")
    }

    fn editor_pointer_drag(
        harness: &mut Harness<'static, WavesPreviewer>,
        start: egui::Pos2,
        end: egui::Pos2,
    ) {
        harness.hover_at(start);
        harness.event(egui::Event::PointerButton {
            pos: start,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: Modifiers::NONE,
        });
        harness.event(egui::Event::PointerMoved(end));
        harness.run_steps(2);
        harness.event(egui::Event::PointerButton {
            pos: end,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        });
        harness.run_steps(2);
    }

    fn editor_amplitude_nav_center_drag(harness: &mut Harness<'static, WavesPreviewer>, dy: f32) {
        let start = editor_amplitude_nav_viewport_rect(harness).center();
        let end = egui::pos2(start.x, start.y + dy);
        editor_pointer_drag(harness, start, end);
    }

    fn editor_amplitude_nav_edge_drag(
        harness: &mut Harness<'static, WavesPreviewer>,
        from_top: bool,
        dy: f32,
    ) {
        let viewport = editor_amplitude_nav_viewport_rect(harness);
        let y = if from_top {
            viewport.top() + 1.0
        } else {
            viewport.bottom() - 1.0
        };
        let start = egui::pos2(viewport.center().x, y);
        let end = egui::pos2(start.x, start.y + dy);
        editor_pointer_drag(harness, start, end);
    }

    fn editor_amplitude_nav_edge_drag_outside_rail(
        harness: &mut Harness<'static, WavesPreviewer>,
        from_top: bool,
        dx: f32,
        dy: f32,
    ) {
        let viewport = editor_amplitude_nav_viewport_rect(harness);
        let y = if from_top {
            viewport.top() + 1.0
        } else {
            viewport.bottom() - 1.0
        };
        let start = egui::pos2(viewport.center().x, y);
        let end = egui::pos2(start.x + dx, start.y + dy);
        editor_pointer_drag(harness, start, end);
    }

    fn editor_amplitude_nav_double_click(harness: &mut Harness<'static, WavesPreviewer>) {
        let pos = editor_amplitude_nav_viewport_rect(harness).center();
        for _ in 0..2 {
            harness.hover_at(pos);
            harness.event(egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            });
            harness.run_steps(1);
            harness.event(egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: Modifiers::NONE,
            });
            harness.run_steps(1);
        }
        harness.run_steps(2);
    }

    fn top_menu_button<'a>(
        harness: &'a Harness<'static, WavesPreviewer>,
        label: &'a str,
    ) -> egui_kittest::Node<'a> {
        let nodes: Vec<_> = harness.query_all_by_label(label).collect();
        let node = nodes
            .into_iter()
            .min_by(|a, b| {
                a.rect()
                    .min
                    .y
                    .partial_cmp(&b.rect().min.y)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .unwrap_or_else(|| panic!("Top menu button '{label}' not found"));
        node
    }

    #[test]
    fn load_folder_shows_files() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        assert!(!harness.state().files.is_empty());
        assert!(harness.state().root.is_some());
    }

    #[test]
    fn top_menu_smoke() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        top_menu_button(&harness, "File");
        top_menu_button(&harness, "Export");
        top_menu_button(&harness, "Tools");
        top_menu_button(&harness, "List");
    }

    #[test]
    fn inspector_panel_visible_when_editor_open() {
        let mut harness = harness_with_wavs(true);
        wait_for_scan(&mut harness);
        wait_for_tab(&mut harness);
        let inspector_nodes: Vec<_> = harness.query_all_by_label("Inspector").collect();
        assert!(!inspector_nodes.is_empty(), "Inspector heading not found");
    }

    #[test]
    fn list_type_badge_column_visible() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        harness.state_mut().list_columns.type_badge = true;
        harness.run_steps(1);
        let type_nodes: Vec<_> = harness.query_all_by_label("Type").collect();
        assert!(!type_nodes.is_empty(), "Type badge header not found");
    }

    #[test]
    fn list_art_column_visible() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        harness.state_mut().list_columns.cover_art = true;
        harness.run_steps(1);
        let art_nodes: Vec<_> = harness.query_all_by_label("Art").collect();
        assert!(!art_nodes.is_empty(), "Art header not found");
    }

    #[test]
    fn list_art_modal_window_visible() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        let wav = first_wav_file(&wav_dir()).expect("wav fixture");
        harness
            .state_mut()
            .test_show_list_art_window_placeholder(&wav);
        harness.run_steps(1);
        let modal_nodes: Vec<_> = harness.query_all_by_label("Artwork").collect();
        assert!(!modal_nodes.is_empty(), "Artwork window title not found");
    }

    #[test]
    fn inspector_tool_combo_reachable() {
        let mut harness = harness_with_wavs(true);
        wait_for_scan(&mut harness);
        wait_for_tab(&mut harness);

        let tool_nodes: Vec<_> = harness.query_all_by_label("Tool").collect();
        assert!(!tool_nodes.is_empty(), "Inspector tool row not found");

        assert!(harness.state_mut().test_set_active_tool(ToolKind::Reverse));
        harness.run_steps(1);
        assert_eq!(harness.state().test_active_tool(), Some(ToolKind::Reverse));
    }

    #[test]
    fn new_editor_tab_inherits_last_opened_inspector_tool() {
        let files = sample_wav_files(2);
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);

        assert!(harness.state_mut().test_open_tab_for_path(&files[0]));
        wait_for_tab_ready(&mut harness);
        assert_eq!(
            harness.state().test_active_tool(),
            Some(ToolKind::LoopEdit),
            "first editor tab should keep the default tool"
        );
        assert!(harness.state_mut().test_set_active_tool(ToolKind::Gain));
        let first_tab_idx = harness.state().active_tab.expect("first tab idx");

        assert!(harness.state_mut().test_open_tab_for_path(&files[1]));
        wait_for_tab_ready(&mut harness);
        assert_eq!(
            harness.state().test_active_tool(),
            Some(ToolKind::Gain),
            "new editor tab should inherit the last opened tab tool"
        );
        assert_eq!(
            harness.state().tabs[first_tab_idx].active_tool,
            ToolKind::Gain,
            "existing tab tool should not be changed while opening a new tab"
        );

        assert!(harness.state_mut().test_set_active_tool(ToolKind::Reverse));
        assert!(harness.state_mut().test_open_tab_for_path(&files[0]));
        harness.run_steps(2);
        assert_eq!(
            harness.state().test_active_tool(),
            Some(ToolKind::Gain),
            "reactivating an existing tab should keep that tab's own tool"
        );
    }

    #[test]
    fn edited_cache_restore_keeps_cached_tool_instead_of_inheriting() {
        let files = sample_wav_files(2);
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);

        assert!(harness.state_mut().test_open_tab_for_path(&files[0]));
        wait_for_tab_ready(&mut harness);
        assert!(harness.state_mut().test_set_active_tool(ToolKind::Reverse));
        assert!(harness.state_mut().test_add_marker_frac(0.25));
        assert!(harness.state_mut().test_close_tab_for_path(&files[0]));
        harness.run_steps(2);

        assert!(harness.state_mut().test_open_tab_for_path(&files[1]));
        wait_for_tab_ready(&mut harness);
        assert!(harness.state_mut().test_set_active_tool(ToolKind::Gain));

        assert!(harness.state_mut().test_open_tab_for_path(&files[0]));
        wait_for_tab_ready(&mut harness);
        assert_eq!(
            harness.state().test_active_tool(),
            Some(ToolKind::Reverse),
            "edited-cache restore should use the cached tab tool, not the inherited tool"
        );
    }

    #[test]
    fn select_row_and_play_pause() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        select_first_row(&mut harness);
        let before = harness
            .state()
            .audio
            .shared
            .playing
            .load(std::sync::atomic::Ordering::Relaxed);
        harness.key_press(Key::Space);
        let start = Instant::now();
        let mut ever_toggled = false;
        loop {
            harness.run_steps(1);
            let after = harness
                .state()
                .audio
                .shared
                .playing
                .load(std::sync::atomic::Ordering::Relaxed);
            if after != before {
                ever_toggled = true;
                break;
            }
            if start.elapsed() > Duration::from_secs(8) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(ever_toggled);
    }

    #[test]
    fn enter_opens_editor_tab() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        select_first_row(&mut harness);
        harness.key_press(Key::Enter);
        wait_for_tab(&mut harness);
        assert!(harness.state().active_tab.is_some());
    }

    #[test]
    fn open_tab_shell_before_deferred_stream_activation() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        let wav = first_wav_file(&wav_dir()).expect("wav fixture");
        assert!(harness.state_mut().test_select_path(&wav));
        harness.run_steps(2);
        assert!(harness.state_mut().test_open_tab_for_path(&wav));

        assert!(
            harness.state().test_is_editor_workspace_active(),
            "editor workspace should become active immediately when opening the selected WAV"
        );
        assert_eq!(
            harness.state().test_active_tab_path().as_deref(),
            Some(wav.as_path()),
            "the selected WAV should open immediately in the editor shell"
        );
        assert!(
            !harness.state().test_audio_is_streaming_wav(&wav),
            "exact-stream activation should be deferred until after the first editor paint"
        );

        let start = Instant::now();
        loop {
            harness.run_steps(1);
            if harness.state().test_audio_is_streaming_wav(&wav) {
                break;
            }
            if start.elapsed() > Duration::from_secs(10) {
                panic!("deferred exact-stream activation timeout");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn enter_opens_editor_with_placeholder_when_meta_is_missing() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        let wav = first_wav_file(&wav_dir()).expect("wav fixture");
        assert!(harness.state_mut().test_select_path(&wav));
        harness.run_steps(2);
        harness.state_mut().test_clear_meta_for_path(&wav);

        harness.key_press(Key::Enter);
        harness.run_steps(1);

        assert!(
            harness.state().test_is_editor_workspace_active(),
            "editor workspace should open even when metadata is unavailable"
        );
        assert_eq!(
            harness.state().test_active_tab_path().as_deref(),
            Some(wav.as_path())
        );
        assert_eq!(
            harness.state().test_active_tab_samples_len_visual(),
            0,
            "initial editor shell should allow an unknown visual length placeholder"
        );

        let start = Instant::now();
        loop {
            harness.run_steps(1);
            if harness.state().test_active_tab_samples_len_visual() > 0
                && harness.state().test_active_tab_loading_waveform_ready()
            {
                break;
            }
            if start.elapsed() > Duration::from_secs(10) {
                panic!("placeholder visual length never updated after decode started");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn loop_toggle_in_editor() {
        let mut harness = harness_with_wavs(true);
        wait_for_scan(&mut harness);
        wait_for_tab(&mut harness);
        let before = format!("{:?}", harness.state().tabs[0].loop_mode);
        harness.key_press(Key::L);
        harness.run_steps(2);
        let after = format!("{:?}", harness.state().tabs[0].loop_mode);
        assert_ne!(before, after);
    }

    #[test]
    fn l_applies_current_loop_markers() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        let tab_idx = harness.state().active_tab.expect("active tab");

        assert!(harness.state_mut().test_set_loop_region_frac(0.20, 0.40));
        let applied = harness
            .state()
            .test_loop_region()
            .expect("applied loop region");
        {
            let tab = &mut harness.state_mut().tabs[tab_idx];
            tab.loop_region_applied = Some(applied);
            tab.loop_region_committed = Some(applied);
            tab.loop_markers_saved = Some(applied);
            tab.loop_mode = neowaves::LoopMode::Off;
        }

        assert!(harness.state_mut().test_set_loop_region_frac(0.55, 0.75));
        {
            let tab = &mut harness.state_mut().tabs[tab_idx];
            tab.pending_loop_unwrap = Some(3);
        }
        let editing = harness
            .state()
            .test_loop_region()
            .expect("editing loop region");

        harness.key_press(Key::L);
        harness.run_steps(2);

        let tab = &harness.state().tabs[tab_idx];
        assert_eq!(tab.loop_region, Some(editing));
        assert_eq!(tab.loop_region_applied, Some(editing));
        assert_eq!(tab.loop_region_committed, Some(editing));
        assert_eq!(tab.loop_mode, neowaves::LoopMode::Marker);
        assert_eq!(tab.pending_loop_unwrap, None);
        assert!(tab.loop_markers_dirty);
    }

    #[test]
    fn editor_loop_visual_ranges_distinguish_applied_and_editing() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        let tab_idx = harness.state().active_tab.expect("active tab");

        assert!(harness.state_mut().test_set_loop_region_frac(0.20, 0.40));
        let applied = harness.state().test_loop_region().expect("applied loop");
        {
            let tab = &mut harness.state_mut().tabs[tab_idx];
            tab.loop_region_applied = Some(applied);
        }
        assert_eq!(harness.state().test_loop_visual_applied_region(), None);
        assert_eq!(
            harness.state().test_loop_visual_editing_region(),
            Some(applied)
        );

        assert!(harness.state_mut().test_set_loop_region_frac(0.55, 0.75));
        let editing = harness.state().test_loop_region().expect("editing loop");
        assert_eq!(
            harness.state().test_loop_visual_applied_region(),
            Some(applied)
        );
        assert_eq!(
            harness.state().test_loop_visual_editing_region(),
            Some(editing)
        );
        assert!(harness.state().test_loop_preview_pending());
    }

    #[test]
    fn mode_buttons_switch() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        harness.get_by_label("Pitch").click();
        harness.run_steps(2);
        assert_eq!(harness.state().test_mode_name(), "PitchShift");
        harness.get_by_label("Stretch").click();
        harness.run_steps(2);
        assert_eq!(harness.state().test_mode_name(), "TimeStretch");
        harness.get_by_label("Speed").click();
        harness.run_steps(2);
        assert_eq!(harness.state().test_mode_name(), "Speed");
    }

    #[test]
    fn open_first_auto_opens_tab() {
        let mut harness = harness_with_wavs(true);
        wait_for_scan(&mut harness);
        wait_for_tab(&mut harness);
        assert!(harness.state().active_tab.is_some());
    }

    #[test]
    fn search_filters_and_clears() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        let initial_len = harness.state().files.len();
        let first_name = path_for_row(harness.state(), 0)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        let query: String = first_name.chars().take(4).collect();
        harness.state_mut().test_set_search_query(&query);
        harness.run_steps(2);
        let filtered_len = harness.state().files.len();
        assert!(filtered_len <= initial_len);
        if !harness.state().files.is_empty() {
            let name = path_for_row(harness.state(), 0)
                .to_string_lossy()
                .to_lowercase();
            assert!(name.contains(&query.to_lowercase()));
        }
        harness.state_mut().test_set_search_query("");
        harness.run_steps(2);
        assert_eq!(harness.state().files.len(), initial_len);
    }

    #[test]
    fn sort_header_cycles() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        harness.state_mut().test_cycle_sort_file();
        assert_eq!(harness.state().test_sort_key_name(), "File");
        assert_eq!(harness.state().test_sort_dir_name(), "Asc");
        harness.state_mut().test_cycle_sort_file();
        assert_eq!(harness.state().test_sort_dir_name(), "Desc");
        harness.state_mut().test_cycle_sort_file();
        assert_eq!(harness.state().test_sort_dir_name(), "None");
    }

    #[test]
    fn shift_arrow_extends_selection() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        select_first_row(&mut harness);
        let mut mods = Modifiers::default();
        mods.shift = true;
        harness.key_press_modifiers(mods, Key::ArrowDown);
        harness.run_steps(2);
        assert!(harness.state().selected_multi.len() >= 2);
    }

    #[test]
    fn loop_markers_set_by_keys() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        harness.state().audio.seek_to_sample(1000);
        harness.key_press(Key::K);
        harness.run_steps(1);
        harness.state().audio.seek_to_sample(2000);
        harness.key_press(Key::P);
        harness.run_steps(2);
        let region = harness.state().tabs[0].loop_region;
        assert!(matches!(region, Some((s, e)) if e > s));
    }

    #[test]
    fn zero_cross_snap_toggles() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        open_first_tab(&mut harness);
        let before = harness.state().tabs[0].snap_zero_cross;
        harness.key_press(Key::R);
        harness.run_steps(2);
        let after = harness.state().tabs[0].snap_zero_cross;
        assert_ne!(before, after);
    }

    #[test]
    fn view_mode_buttons_switch() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        open_first_tab(&mut harness);
        let cases = [
            (neowaves::ViewMode::Spectrogram, "Spec", "Spectrogram"),
            (neowaves::ViewMode::Log, "Freq Log", "Log"),
            (neowaves::ViewMode::Mel, "Mel", "Mel"),
            (neowaves::ViewMode::Tempogram, "Tempogram", "Tempogram"),
            (neowaves::ViewMode::Chromagram, "Chromagram", "Chromagram"),
            (neowaves::ViewMode::Waveform, "Wave", "Waveform"),
        ];
        for (mode, combo_value, debug_name) in cases {
            assert!(harness.state_mut().test_set_view_mode(mode));
            harness.run_steps(2);
            assert_eq!(
                format!("{:?}", harness.state().tabs[0].leaf_view_mode()),
                debug_name
            );
            assert!(
                harness
                    .query_all_by_value(combo_value)
                    .any(|node| node.accesskit_node().role() == egui::accesskit::Role::ComboBox),
                "view selector should show {combo_value}"
            );
        }
    }

    #[test]
    fn view_mode_hotkey_cycles_across_other_views() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        let expected = [
            "Spectrogram",
            "Log",
            "Mel",
            "Tempogram",
            "Chromagram",
            "Waveform",
        ];
        for expected_view in expected {
            harness.key_press(Key::S);
            harness.run_steps(2);
            assert_eq!(
                format!(
                    "{:?}",
                    harness.state().tabs[harness.state().active_tab.unwrap()].leaf_view_mode()
                ),
                expected_view
            );
        }
    }

    #[test]
    fn view_switch_keeps_editor_playback_running() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);

        harness.key_press(Key::Space);
        harness.run_steps(3);
        assert!(
            harness.state().test_audio_is_playing(),
            "playback should start"
        );
        let transport_before = harness.state().test_playback_transport_name().to_string();
        let sr_before = harness.state().test_playback_transport_sr();

        assert!(harness
            .state_mut()
            .test_set_view_mode(neowaves::ViewMode::Spectrogram));
        harness.run_steps(2);
        assert!(
            harness.state().test_audio_is_playing(),
            "playback should continue after Spec switch"
        );
        assert_eq!(
            harness.state().test_playback_transport_name(),
            transport_before
        );
        assert_eq!(harness.state().test_playback_transport_sr(), sr_before);

        assert!(harness
            .state_mut()
            .test_set_view_mode(neowaves::ViewMode::Tempogram));
        harness.run_steps(2);
        assert!(
            harness.state().test_audio_is_playing(),
            "playback should continue after Other switch"
        );
        assert_eq!(
            harness.state().test_playback_transport_name(),
            transport_before
        );
        assert_eq!(harness.state().test_playback_transport_sr(), sr_before);

        assert!(harness
            .state_mut()
            .test_set_view_mode(neowaves::ViewMode::Chromagram));
        harness.run_steps(2);
        assert!(
            harness.state().test_audio_is_playing(),
            "playback should continue after Chromagram switch"
        );
        assert_eq!(
            harness.state().test_playback_transport_name(),
            transport_before
        );
        assert_eq!(harness.state().test_playback_transport_sr(), sr_before);
    }

    #[test]
    fn loop_edit_buttons_set_region() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness.state_mut().test_set_loop_region_frac(0.2, 0.6));
        harness.run_steps(2);
        let region = harness.state().tabs[0].loop_region;
        assert!(matches!(region, Some((s, e)) if e > s));
    }

    #[test]
    fn clear_gains_from_menu() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        select_first_row(&mut harness);
        harness.key_press(Key::ArrowRight);
        harness.run_steps(2);
        assert!(harness.state().test_pending_gain_count() > 0);
        harness.get_by_label("Export").click();
        harness.run_steps(1);
        harness.get_by_label("Clear All Gains").click();
        harness.run_steps(2);
        assert_eq!(harness.state().test_pending_gain_count(), 0);
    }

    #[test]
    fn add_paths_avoids_duplicates() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        let before = harness.state().items.len();
        let path = harness.state().items[0].path.clone();
        let added = harness.state_mut().test_add_paths(&[path]);
        harness.run_steps(2);
        assert_eq!(added, 0);
        assert_eq!(harness.state().items.len(), before);
    }

    #[test]
    fn replace_with_files_clears_root() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        let files = harness
            .state()
            .items
            .iter()
            .take(2)
            .map(|item| item.path.clone())
            .collect::<Vec<_>>();
        harness.state_mut().test_replace_with_files(&files);
        harness.run_steps(2);
        assert!(harness.state().root.is_none());
        assert_eq!(harness.state().items.len(), files.len());
    }

    #[test]
    fn gain_adjust_with_arrows() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        let path = select_first_row(&mut harness);
        harness.key_press(Key::ArrowRight);
        harness.run_steps(2);
        assert!(harness.state().test_has_pending_gain(&path));
    }

    #[test]
    fn export_settings_opens() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        harness.get_by_label("Tools").click();
        harness.run_steps(1);
        harness.get_by_label("Settings...").click();
        harness.run_steps(2);
        assert!(harness.state().test_show_export_settings());
    }

    #[test]
    fn ctrl_a_selects_all_rows() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        let mut mods = Modifiers::default();
        mods.ctrl = true;
        harness.key_press_modifiers(mods, Key::A);
        harness.run_steps(2);
        let state = harness.state();
        assert_eq!(state.selected_multi.len(), state.files.len());
    }

    #[test]
    fn list_shortcut_p_toggles_auto_play() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        let before = harness.state().test_auto_play_list_nav();
        harness.key_press(Key::P);
        harness.run_steps(2);
        let after = harness.state().test_auto_play_list_nav();
        assert_ne!(before, after);
    }

    #[test]
    fn auto_play_pref_roundtrip_persists() {
        let mut harness = harness_empty();
        let prefs = make_temp_dir("prefs_autoplay").join("prefs.txt");
        harness.state_mut().test_set_auto_play_list_nav(true);
        harness.state().test_save_prefs_to_path(&prefs);
        harness.state_mut().test_set_auto_play_list_nav(false);
        harness.state_mut().test_load_prefs_from_path(&prefs);
        assert!(harness.state().test_auto_play_list_nav());

        harness.state_mut().test_set_auto_play_list_nav(false);
        harness.state().test_save_prefs_to_path(&prefs);
        harness.state_mut().test_set_auto_play_list_nav(true);
        harness.state_mut().test_load_prefs_from_path(&prefs);
        assert!(!harness.state().test_auto_play_list_nav());
    }

    #[test]
    fn startup_open_files_selects_last_target_and_sets_autoplay() {
        let files = first_n_audio_files(&wav_dir(), 3);
        assert!(files.len() >= 3, "expected at least 3 audio files");
        let mut harness = harness_empty();
        harness.state_mut().test_set_auto_play_list_nav(true);
        harness.state_mut().test_apply_startup_open_files(&files);
        wait_for_tab(&mut harness);
        harness.run_steps(2);

        let selected = harness
            .state()
            .test_selected_path()
            .cloned()
            .expect("selected startup path");
        let active_tab = harness
            .state()
            .test_active_tab_path()
            .expect("startup active editor path");
        assert_eq!(
            selected, files[2],
            "startup should select the last opened file"
        );
        assert_eq!(
            active_tab, files[2],
            "startup shell-open should open the last file in editor"
        );
        assert!(
            harness.state().test_is_editor_workspace_active(),
            "startup shell-open should switch to editor workspace"
        );
        assert!(
            harness.state().test_pending_editor_autoplay_path() == Some(files[2].clone())
                || harness.state().test_audio_is_playing(),
            "startup open with autoplay should schedule or start editor playback"
        );
    }

    #[test]
    fn append_open_files_opens_last_target_in_editor_and_duplicate_reselects_existing_row() {
        let files = first_n_audio_files(&wav_dir(), 3);
        assert!(files.len() >= 3, "expected at least 3 audio files");
        let mut harness = harness_empty();
        harness.state_mut().test_set_auto_play_list_nav(true);
        let added = harness
            .state_mut()
            .test_append_open_files_and_open_editor(&files[..2], true);
        assert_eq!(added, 2);
        wait_for_tab(&mut harness);
        harness.run_steps(2);

        let selected = harness
            .state()
            .test_selected_path()
            .cloned()
            .expect("selected appended path");
        let active_tab = harness
            .state()
            .test_active_tab_path()
            .expect("active tab after append");
        assert_eq!(
            selected, files[1],
            "append should select the last opened file"
        );
        assert_eq!(
            active_tab, files[1],
            "append shell-open should open the last file in editor"
        );

        harness.state_mut().test_set_auto_play_list_nav(false);
        let added_dup = harness
            .state_mut()
            .test_append_open_files_and_open_editor(&[files[0].clone()], true);
        assert_eq!(added_dup, 0, "duplicate reopen should not append a new row");
        harness.run_steps(2);
        let reselection = harness
            .state()
            .test_selected_path()
            .cloned()
            .expect("selected duplicate path");
        let reactivated_tab = harness
            .state()
            .test_active_tab_path()
            .expect("active tab after duplicate reopen");
        assert_eq!(
            reselection, files[0],
            "duplicate reopen should reselect the existing row"
        );
        assert_eq!(
            reactivated_tab, files[0],
            "duplicate reopen should reactivate the existing editor tab"
        );
    }

    #[test]
    fn list_shortcut_a_d_adjust_volume() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        let base = harness.state().test_volume_db();
        harness.key_press(Key::A);
        harness.run_steps(1);
        let down = harness.state().test_volume_db();
        assert!(down < base);
        harness.key_press(Key::D);
        harness.run_steps(1);
        let up = harness.state().test_volume_db();
        assert!(up > down);
    }

    #[test]
    fn list_playback_continuity_for_formats() {
        let dir = make_temp_dir("list_play_formats");
        let formats = build_format_fixtures(&dir, 4.0);
        let mut harness = harness_with_folder(dir.clone());
        wait_for_scan(&mut harness);
        assert!(
            harness.state().files.len() >= formats.len(),
            "expected at least {} files in list",
            formats.len()
        );

        for row in 0..formats.len() {
            harness.state_mut().audio.stop();
            assert!(
                harness.state_mut().test_select_and_load_row(row),
                "failed to select row {row}"
            );
            let selected = harness
                .state()
                .test_selected_path()
                .cloned()
                .expect("selected path");
            let _ = harness
                .state_mut()
                .test_force_load_selected_list_preview_for_play();

            let mut ready = false;
            for _ in 0..200 {
                harness.run_steps(1);
                let state = harness.state();
                let selected_matches = state
                    .test_playing_path()
                    .map(|p| p == &selected)
                    .unwrap_or(false);
                if selected_matches
                    && state.test_audio_has_samples()
                    && state.test_audio_is_playing()
                {
                    ready = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(
                ready,
                "playback did not start in time for {}",
                selected.display()
            );

            let info = neowaves::audio_io::read_audio_info(&selected).ok();
            let sr = info.map(|i| i.sample_rate).unwrap_or(0);
            let initial_len = audio_buffer_len(harness.state());
            let mut max_len = initial_len;
            let transport = harness.state().test_playback_transport_name().to_string();
            for _ in 0..160 {
                harness.run_steps(1);
                let len = audio_buffer_len(harness.state());
                if len > max_len {
                    max_len = len;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            let already_long = transport == "ExactStreamWav"
                || (sr > 0 && initial_len >= (sr as f32 * 3.0) as usize);
            assert!(
                max_len > initial_len || already_long,
                "list preview buffer did not grow for {} (initial={} max={} sr={} transport={transport})",
                selected.display(),
                initial_len,
                max_len,
                sr
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[ignore = "manual perf measurement"]
    fn list_navigation_timing_metrics() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        select_first_row(&mut harness);
        let steps = 120usize;
        let start = Instant::now();
        for _ in 0..steps {
            harness.key_press(Key::ArrowDown);
            harness.run_steps(1);
        }
        let elapsed = start.elapsed();
        let per_ms = elapsed.as_secs_f64() * 1000.0 / steps as f64;
        eprintln!(
            "list_navigation_timing_metrics: steps={} total_ms={:.2} per_step_ms={:.2}",
            steps,
            elapsed.as_secs_f64() * 1000.0,
            per_ms
        );
    }

    #[test]
    #[ignore = "manual perf measurement"]
    fn list_select_and_load_call_timing_metrics() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        let rows = harness.state().files.len();
        let steps = 120usize.min(rows.saturating_sub(1));
        let start = Instant::now();
        for i in 0..steps {
            let row = (i + 1).min(rows.saturating_sub(1));
            assert!(harness.state_mut().test_select_and_load_row(row));
        }
        let elapsed = start.elapsed();
        let per_ms = elapsed.as_secs_f64() * 1000.0 / steps.max(1) as f64;
        eprintln!(
            "list_select_and_load_call_timing_metrics: steps={} total_ms={:.2} per_call_ms={:.2}",
            steps,
            elapsed.as_secs_f64() * 1000.0,
            per_ms
        );
    }

    #[test]
    #[ignore = "manual perf measurement"]
    fn list_idle_frame_timing_metrics() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        let steps = 120usize;
        let start = Instant::now();
        for _ in 0..steps {
            harness.run_steps(1);
        }
        let elapsed = start.elapsed();
        let per_ms = elapsed.as_secs_f64() * 1000.0 / steps as f64;
        eprintln!(
            "list_idle_frame_timing_metrics: steps={} total_ms={:.2} per_frame_ms={:.2}",
            steps,
            elapsed.as_secs_f64() * 1000.0,
            per_ms
        );
    }

    #[test]
    #[ignore = "manual perf measurement"]
    fn list_sync_decode_timing_reference() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        let rows = harness.state().files.len();
        let steps = 32usize.min(rows.saturating_sub(1));
        let start = Instant::now();
        for i in 0..steps {
            let row = (i + 1).min(rows.saturating_sub(1));
            assert!(harness.state_mut().test_select_and_load_row(row));
            let _ = harness
                .state_mut()
                .test_force_load_selected_list_preview_for_play();
        }
        let elapsed = start.elapsed();
        let per_ms = elapsed.as_secs_f64() * 1000.0 / steps.max(1) as f64;
        eprintln!(
            "list_sync_decode_timing_reference: steps={} total_ms={:.2} per_call_ms={:.2}",
            steps,
            elapsed.as_secs_f64() * 1000.0,
            per_ms
        );
    }

    #[test]
    #[ignore = "manual perf measurement"]
    fn list_autoplay_ready_timing_metrics() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        harness.state_mut().test_set_auto_play_list_nav(true);
        select_first_row(&mut harness);
        harness.run_steps(2);

        let rows = harness.state().files.len();
        let steps = 48usize.min(rows.saturating_sub(1));
        if steps == 0 {
            eprintln!("list_autoplay_ready_timing_metrics: skipped (not enough rows)");
            return;
        }

        let mut lat_ms: Vec<f64> = Vec::new();
        let mut timeouts = 0usize;
        for _ in 0..steps {
            harness.key_press(Key::ArrowDown);
            let start = Instant::now();
            let mut ready = false;
            for _ in 0..120 {
                harness.run_steps(1);
                let state = harness.state();
                let selected = state.test_selected_path().cloned();
                let playing = state.test_playing_path().cloned();
                if selected.is_some()
                    && selected == playing
                    && state.test_audio_is_playing()
                    && state.test_audio_has_samples()
                {
                    ready = true;
                    break;
                }
            }
            if ready {
                lat_ms.push(start.elapsed().as_secs_f64() * 1000.0);
            } else {
                timeouts = timeouts.saturating_add(1);
            }
        }

        lat_ms.sort_by(|a, b| a.total_cmp(b));
        let avg = if lat_ms.is_empty() {
            0.0
        } else {
            lat_ms.iter().sum::<f64>() / lat_ms.len() as f64
        };
        let p95 = if lat_ms.is_empty() {
            0.0
        } else {
            lat_ms[((lat_ms.len() - 1) * 95) / 100]
        };
        let max = lat_ms.last().copied().unwrap_or(0.0);
        eprintln!(
            "list_autoplay_ready_timing_metrics: steps={} measured={} timeouts={} avg_ms={:.2} p95_ms={:.2} max_ms={:.2}",
            steps,
            lat_ms.len(),
            timeouts,
            avg,
            p95,
            max
        );
    }

    #[test]
    fn arrow_down_moves_selection() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        select_first_row(&mut harness);
        let before = harness.state().selected;
        harness.key_press(Key::ArrowDown);
        harness.run_steps(2);
        let after = harness.state().selected;
        assert_ne!(before, after);
    }

    #[test]
    fn choose_folder_dialog_uses_queue() {
        let mut harness = harness_empty();
        let dir = wav_dir();
        harness
            .state_mut()
            .test_queue_folder_dialog(Some(dir.clone()));
        top_menu_button(&harness, "File").click();
        harness.run_steps(1);
        harness.get_by_label("Folder...").click();
        wait_for_scan(&mut harness);
        assert_eq!(harness.state().root.as_ref(), Some(&dir));
        assert!(!harness.state().files.is_empty());
    }

    #[test]
    fn choose_files_dialog_uses_queue() {
        let mut harness = harness_empty();
        let files = sample_wav_files(2);
        harness
            .state_mut()
            .test_queue_files_dialog(Some(files.clone()));
        top_menu_button(&harness, "File").click();
        harness.run_steps(1);
        harness.get_by_label("Files...").click();
        harness.run_steps(2);
        assert!(harness.state().root.is_none());
        assert_eq!(harness.state().items.len(), files.len());
    }

    #[test]
    fn drag_drop_folder_adds_files() {
        let mut harness = harness_empty();
        let dir = wav_dir();
        let added = harness.state_mut().test_simulate_drop_paths(&[dir]);
        harness.run_steps(2);
        assert!(added > 0);
        assert_eq!(harness.state().items.len(), added);
        assert!(harness.state().root.is_none());
    }

    #[test]
    fn editor_trim_reduces_length() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        let (before, expected_start_sample, expected_first_sample) = {
            let state = harness.state();
            let tab_idx = state.active_tab.expect("active tab");
            let tab = &state.tabs[tab_idx];
            let before = tab.samples_len;
            let expected_start_sample = ((before as f32) * 0.1).floor() as usize;
            let expected_first_sample = tab
                .ch_samples
                .first()
                .and_then(|ch| ch.get(expected_start_sample))
                .copied()
                .unwrap_or(0.0);
            (before, expected_start_sample, expected_first_sample)
        };
        assert!(harness.state_mut().test_apply_trim_frac(0.1, 0.9));
        harness.run_steps(2);
        let state = harness.state();
        let tab_idx = state.active_tab.expect("active tab");
        let tab = &state.tabs[tab_idx];
        let after = tab.samples_len;
        assert!(after < before);
        assert!(
            tab.trim_range.is_none(),
            "trim range should clear after apply"
        );
        assert!(
            tab.selection.is_none(),
            "selection should clear after apply trim"
        );
        let first_after = tab
            .ch_samples
            .first()
            .and_then(|ch| ch.first())
            .copied()
            .unwrap_or(0.0);
        assert!(
            (first_after - expected_first_sample).abs() < 1.0e-6,
            "trim should keep the selected start as the new first sample (start={}, got={}, expected={})",
            expected_start_sample,
            first_after,
            expected_first_sample
        );
        assert!(tab.waveform_pyramid.is_some());
        assert!(harness.state().test_tab_dirty());
    }

    #[test]
    fn editor_fade_in_out_marks_dirty() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness
            .state_mut()
            .test_apply_fade_in(0.0, 0.2, neowaves::FadeShape::SCurve));
        assert!(harness
            .state_mut()
            .test_apply_fade_out(0.8, 1.0, neowaves::FadeShape::SCurve));
        harness.run_steps(2);
        assert!(harness.state().test_tab_dirty());
    }

    #[test]
    fn editor_gain_and_normalize() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness.state_mut().test_apply_gain(0.2, 0.6, -6.0));
        assert!(harness.state_mut().test_apply_normalize(0.0, 1.0, -3.0));
        harness.run_steps(2);
        assert!(harness.state().test_tab_dirty());
    }

    #[test]
    fn editor_reverse_marks_dirty() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness.state_mut().test_apply_reverse(0.1, 0.4));
        harness.run_steps(2);
        assert!(harness.state().test_tab_dirty());
    }

    #[test]
    fn editor_markers_add_and_clear() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness.state_mut().test_add_marker_frac(0.2));
        assert!(harness.state_mut().test_add_marker_frac(0.8));
        assert!(harness.state().test_marker_count() >= 2);
        assert!(harness.state_mut().test_clear_markers());
        assert_eq!(harness.state().test_marker_count(), 0);
    }

    #[test]
    fn editor_loop_region_and_mode() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness.state_mut().test_set_loop_region_frac(0.2, 0.6));
        assert!(harness
            .state_mut()
            .test_set_loop_xfade_ms(40.0, neowaves::LoopXfadeShape::EqualPower));
        assert!(harness
            .state_mut()
            .test_set_loop_mode(neowaves::LoopMode::Marker));
        assert!(harness.state().test_loop_marker_dirty());
        assert!(harness.state().test_loop_preview_pending());
        harness.run_steps(2);
        let region = harness.state().test_loop_region();
        assert!(matches!(region, Some((s, e)) if e > s));
    }

    #[test]
    fn list_wave_overlay_prefers_open_tab_live_state() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        let path = harness
            .state()
            .test_active_tab_path()
            .expect("active tab path");

        assert!(harness.state_mut().test_set_list_wave_meta_annotations(
            &path,
            vec![0.05, 0.95],
            Some((0.10, 0.90)),
        ));
        assert!(harness.state_mut().test_add_marker_frac(0.25));
        assert!(harness.state_mut().test_add_marker_frac(0.75));
        assert!(harness.state_mut().test_set_loop_region_frac(0.20, 0.40));

        let loop_frac = harness
            .state()
            .test_list_wave_loop_frac(&path)
            .expect("resolved live loop frac");
        assert_eq!(
            harness.state().test_list_wave_marker_frac_count(&path),
            Some(2)
        );
        assert!(
            (loop_frac.0 - 0.20).abs() < 0.03 && (loop_frac.1 - 0.40).abs() < 0.03,
            "expected live loop frac, got {:?}",
            loop_frac
        );
        assert!(harness.state().test_list_wave_overlay_dirty(&path));
    }

    #[test]
    fn list_wave_overlay_empty_live_state_hides_baseline_annotations() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        let path = harness
            .state()
            .test_active_tab_path()
            .expect("active tab path");

        assert!(harness.state_mut().test_set_list_wave_meta_annotations(
            &path,
            vec![0.15, 0.50, 0.85],
            Some((0.20, 0.80)),
        ));

        assert_eq!(
            harness.state().test_list_wave_marker_frac_count(&path),
            Some(0)
        );
        assert_eq!(harness.state().test_list_wave_loop_frac(&path), None);
    }

    #[test]
    fn list_wave_overlay_prefers_cached_edits_over_baseline() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        let path = harness
            .state()
            .test_active_tab_path()
            .expect("active tab path");

        assert!(harness.state_mut().test_set_list_wave_meta_annotations(
            &path,
            vec![0.05, 0.95],
            Some((0.10, 0.90)),
        ));
        assert!(harness.state_mut().test_add_marker_frac(0.30));
        assert!(harness.state_mut().test_set_loop_region_frac(0.35, 0.65));
        assert!(harness.state_mut().test_close_tab_for_path(&path));
        harness.run_steps(2);

        let loop_frac = harness
            .state()
            .test_list_wave_loop_frac(&path)
            .expect("resolved cached loop frac");
        assert_eq!(
            harness.state().test_list_wave_marker_frac_count(&path),
            Some(1)
        );
        assert!(
            (loop_frac.0 - 0.35).abs() < 0.03 && (loop_frac.1 - 0.65).abs() < 0.03,
            "expected cached loop frac, got {:?}",
            loop_frac
        );
        assert!(harness.state().test_list_wave_overlay_dirty(&path));
    }

    #[test]
    fn list_wave_overlay_marker_coalescing_is_pixel_bounded() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        let path = first_wav_file(&wav_dir()).expect("wav fixture");
        assert!(harness.state_mut().test_set_list_wave_meta_annotations(
            &path,
            (0..256).map(|i| i as f32 / 255.0).collect(),
            Some((0.10, 0.90)),
        ));

        let raw = harness
            .state()
            .test_list_wave_marker_frac_count(&path)
            .expect("raw overlay");
        let coalesced = harness
            .state()
            .test_list_wave_coalesced_marker_count(&path, 12.0)
            .expect("coalesced overlay");
        assert!(raw > 12);
        assert!(coalesced <= 12, "coalesced marker count should fit width");
    }

    #[test]
    fn editor_loop_xfade_works_at_file_edges() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness.state_mut().test_set_loop_region_frac(0.0, 1.0));
        assert!(harness
            .state_mut()
            .test_set_loop_xfade_ms(40.0, neowaves::LoopXfadeShape::EqualPowerDip));
        assert!(harness
            .state_mut()
            .test_set_loop_mode(neowaves::LoopMode::Marker));
        harness.run_steps(2);
        assert!(harness.state().test_audio_loop_xfade_samples() > 0);
    }

    #[test]
    fn editor_pitch_shift_apply() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness.state_mut().test_apply_pitch_shift(4.0));
        wait_for_editor_apply(&mut harness);
        assert!(harness.state().test_tab_dirty());
    }

    #[test]
    fn editor_time_stretch_apply() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness.state_mut().test_apply_time_stretch(1.2));
        wait_for_editor_apply(&mut harness);
        assert!(harness.state().test_tab_dirty());
    }

    #[test]
    fn editor_view_mode_and_overlay_toggle() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        let active = harness.state().active_tab.expect("active tab");
        assert!(
            !harness.state().tabs[active].show_waveform_overlay,
            "new editor tabs should default waveform overlay off"
        );
        assert!(harness
            .state_mut()
            .test_set_view_mode(neowaves::ViewMode::Spectrogram));
        harness.run_steps(1);
        assert_eq!(
            format!(
                "{:?}",
                harness.state().tabs[harness.state().active_tab.unwrap()].leaf_view_mode()
            ),
            "Spectrogram"
        );
        assert!(
            !harness.state().tabs[harness.state().active_tab.unwrap()].show_waveform_overlay,
            "spec should inherit the non-wave default"
        );
        for mode in [
            neowaves::ViewMode::Log,
            neowaves::ViewMode::Mel,
            neowaves::ViewMode::Tempogram,
            neowaves::ViewMode::Chromagram,
        ] {
            assert!(harness.state_mut().test_set_view_mode(mode));
            harness.run_steps(1);
            assert!(
                !harness.state().tabs[harness.state().active_tab.unwrap()].show_waveform_overlay,
                "new tabs should keep waveform overlay off for {mode:?}"
            );
        }
        assert!(harness
            .state_mut()
            .test_set_view_mode(neowaves::ViewMode::Mel));
        assert!(harness.state_mut().test_set_waveform_overlay(true));
        harness.run_steps(1);
        assert_eq!(
            format!(
                "{:?}",
                harness.state().tabs[harness.state().active_tab.unwrap()].leaf_view_mode()
            ),
            "Mel"
        );
        assert!(harness
            .state_mut()
            .test_set_view_mode(neowaves::ViewMode::Chromagram));
        harness.run_steps(1);
        assert!(
            harness.state().tabs[harness.state().active_tab.unwrap()].show_waveform_overlay,
            "explicit overlay choice should survive view switching"
        );
    }

    #[test]
    fn loop_inspector_shows_three_windows() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        harness.state().audio.seek_to_sample(1200);
        harness.key_press(Key::K);
        harness.run_steps(1);
        harness.state().audio.seek_to_sample(7200);
        harness.key_press(Key::P);
        harness.run_steps(3);

        assert!(!harness
            .query_all_by_label("Pre-Loop window")
            .collect::<Vec<_>>()
            .is_empty());
        assert!(!harness
            .query_all_by_label("Seam preview")
            .collect::<Vec<_>>()
            .is_empty());
        assert!(!harness
            .query_all_by_label("Post-Loop window")
            .collect::<Vec<_>>()
            .is_empty());
    }

    #[test]
    fn editor_ctrl_wheel_zoom_in_changes_samples_per_px() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        harness.run_steps(2);

        let tab_idx = harness.state().active_tab.expect("active tab");
        let spp_before = harness.state().tabs[tab_idx].samples_per_px;
        assert!(spp_before > 0.0, "samples_per_px should be initialized");

        editor_zoom_in_once(&mut harness);

        let spp_after = harness.state().tabs[tab_idx].samples_per_px;
        assert!(
            spp_after < spp_before,
            "ctrl+wheel zoom in should reduce samples_per_px: before={spp_before} after={spp_after}"
        );
    }

    #[test]
    fn editor_open_initializes_waveform_geometry_without_zoom_nudge() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        harness.run_steps(2);

        let tab_idx = harness.state().active_tab.expect("active tab");
        let tab = &harness.state().tabs[tab_idx];
        assert!(
            tab.samples_per_px > 0.0,
            "samples_per_px should be ready after open"
        );
        assert!(
            tab.last_wave_w > 0.0,
            "last_wave_w should be ready after open"
        );
        let display_len = harness
            .state()
            .test_editor_display_samples_len()
            .expect("display length");
        let (start, end) = harness
            .state()
            .test_editor_visible_display_range()
            .expect("visible display range");
        assert!(display_len > 0, "display length should be non-zero");
        assert!(
            start < end,
            "visible range should be non-empty: {start}..{end}"
        );
        assert!(
            end <= display_len,
            "visible range should fit display length: end={end} len={display_len}"
        );
        assert!(
            tab.view_offset <= display_len.saturating_sub(1),
            "view offset should be clamped after open"
        );
    }

    #[test]
    fn editor_resize_refits_when_whole_file_is_visible() {
        let mut harness = harness_with_dynamic_editor_fixture();
        harness.set_size(egui::vec2(900.0, 720.0));
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        harness.run_steps(3);
        assert_editor_whole_fit(&harness, "before resize");
        let before_wave_w = editor_wave_width(&harness);

        harness.set_size(egui::vec2(1920.0, 720.0));
        harness.run_steps(6);
        let after_wave_w = editor_wave_width(&harness);
        assert!(
            after_wave_w > before_wave_w + 100.0,
            "test setup should widen the editor canvas: before={before_wave_w} after={after_wave_w}"
        );
        assert_editor_whole_fit(&harness, "after resize");

        let display_len = harness
            .state()
            .test_editor_display_samples_len()
            .expect("display length");
        let last_x = harness
            .state()
            .test_editor_display_sample_x_offset(display_len.saturating_sub(1))
            .expect("last sample x");
        let wave_w = editor_wave_width(&harness);
        assert!(
            last_x >= wave_w - 2.0,
            "last sample should reach the right edge after fit resize: x={last_x} wave_w={wave_w}"
        );
    }

    #[test]
    fn editor_resize_preserves_center_when_zoomed_in() {
        let mut harness = harness_with_dynamic_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        for _ in 0..8 {
            editor_zoom_in_once(&mut harness);
        }
        harness.run_steps(2);

        let tab_idx = harness.state().active_tab.expect("active tab");
        let before_spp = harness.state().tabs[tab_idx].samples_per_px;
        let before_center = editor_center_display_sample(&harness);
        let before_wave_w = editor_wave_width(&harness);

        harness.set_size(egui::vec2(1600.0, 720.0));
        harness.run_steps(6);

        let after = &harness.state().tabs[tab_idx];
        let after_center = editor_center_display_sample(&harness);
        let after_wave_w = editor_wave_width(&harness);
        assert!(
            after_wave_w > before_wave_w + 100.0,
            "test setup should widen the editor canvas: before={before_wave_w} after={after_wave_w}"
        );
        assert!(
            (after.samples_per_px - before_spp).abs() <= before_spp.max(1.0) * 0.01,
            "zoomed resize should preserve zoom level: before={before_spp} after={}",
            after.samples_per_px
        );
        assert!(
            (after_center as i64 - before_center as i64).abs() <= 4,
            "zoomed resize should preserve center sample: before={before_center} after={after_center}"
        );
    }

    #[test]
    fn editor_plain_vertical_wheel_zoom_in_changes_samples_per_px() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        harness.run_steps(2);

        let tab_idx = harness.state().active_tab.expect("active tab");
        let spp_before = harness.state().tabs[tab_idx].samples_per_px;
        editor_plain_vertical_wheel_once(&mut harness);
        let spp_after = harness.state().tabs[tab_idx].samples_per_px;
        assert!(
            spp_after < spp_before,
            "plain vertical wheel should zoom in: before={spp_before} after={spp_after}"
        );
    }

    #[test]
    fn editor_horizontal_wheel_pan_changes_view_offset_without_shift() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        for _ in 0..8 {
            editor_zoom_in_once(&mut harness);
        }
        let tab_idx = harness.state().active_tab.expect("active tab");
        let mid_view = harness.state().tabs[tab_idx].samples_len / 2;
        assert!(harness.state_mut().test_set_tab_view_offset(mid_view));
        harness.run_steps(1);

        let before_view = harness.state().tabs[tab_idx].view_offset;
        let before_exact = harness.state().tabs[tab_idx].view_offset_exact;
        let before_spp = harness.state().tabs[tab_idx].samples_per_px;
        editor_horizontal_pan_once(&mut harness, 120.0);
        let after = &harness.state().tabs[tab_idx];
        assert!(
            after.view_offset != before_view
                || (after.view_offset_exact - before_exact).abs() > 0.001,
            "horizontal wheel should pan without Shift"
        );
        assert!(
            (after.samples_per_px - before_spp).abs() < 0.0001,
            "horizontal wheel pan should not zoom"
        );
    }

    #[test]
    fn editor_shift_wheel_pan_changes_view_offset() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        editor_zoom_in_once(&mut harness);

        let tab_idx = harness.state().active_tab.expect("active tab");
        let before = harness.state().tabs[tab_idx].view_offset;
        editor_shift_pan_once(&mut harness);

        let after = harness.state().tabs[tab_idx].view_offset;
        assert_ne!(after, before, "Shift+wheel should pan the editor view");
    }

    #[test]
    fn editor_zoom_then_pan_then_zoom_preserves_anchor_reasonably() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);

        editor_zoom_in_once(&mut harness);
        editor_shift_pan_once(&mut harness);

        let tab_idx = harness.state().active_tab.expect("active tab");
        let before_second_zoom_spp = harness.state().tabs[tab_idx].samples_per_px;
        let view_before_second_zoom = harness.state().tabs[tab_idx].view_offset as i64;
        let visible_before_second_zoom =
            (before_second_zoom_spp * editor_wave_width(&harness)).round() as i64;

        editor_zoom_in_once(&mut harness);

        let after_second_zoom = &harness.state().tabs[tab_idx];
        let delta = (after_second_zoom.view_offset as i64 - view_before_second_zoom).abs();
        assert!(
            after_second_zoom.samples_per_px < before_second_zoom_spp,
            "second zoom should still zoom in"
        );
        assert!(
            delta < visible_before_second_zoom.max(256),
            "zoom after pan should keep anchor reasonably stable: delta={delta} visible={visible_before_second_zoom}"
        );
    }

    #[test]
    fn editor_middle_drag_pan_changes_view_offset() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        editor_zoom_in_once(&mut harness);

        let tab_idx = harness.state().active_tab.expect("active tab");
        let before = harness.state().tabs[tab_idx].view_offset;
        let start = editor_canvas_hover_pos(&harness);
        let end = egui::pos2(start.x + 140.0, start.y);
        harness.hover_at(start);
        harness.event_modifiers(
            egui::Event::PointerButton {
                pos: start,
                button: egui::PointerButton::Middle,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
            Modifiers::NONE,
        );
        harness.event_modifiers(egui::Event::PointerMoved(end), Modifiers::NONE);
        harness.run_steps(2);
        harness.event_modifiers(
            egui::Event::PointerButton {
                pos: end,
                button: egui::PointerButton::Middle,
                pressed: false,
                modifiers: Modifiers::NONE,
            },
            Modifiers::NONE,
        );
        harness.run_steps(2);

        let after = harness.state().tabs[tab_idx].view_offset;
        assert_ne!(after, before, "Middle drag should pan the editor view");
    }

    #[test]
    fn editor_high_zoom_shift_wheel_pan_does_not_stall() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        for _ in 0..10 {
            editor_zoom_in_once(&mut harness);
        }
        let tab_idx = harness.state().active_tab.expect("active tab");
        let mid_view = harness.state().tabs[tab_idx].samples_len / 2;
        assert!(harness.state_mut().test_set_tab_view_offset(mid_view));
        harness.run_steps(1);
        let before = harness
            .state()
            .test_tab_view_offset()
            .expect("view offset before");
        for _ in 0..4 {
            editor_shift_pan_once(&mut harness);
        }
        let after = harness
            .state()
            .test_tab_view_offset()
            .expect("view offset after");
        assert_ne!(after, before, "high zoom shift+wheel pan should not stall");
    }

    #[test]
    fn editor_high_zoom_middle_drag_pan_does_not_stall() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        for _ in 0..10 {
            editor_zoom_in_once(&mut harness);
        }
        let tab_idx = harness.state().active_tab.expect("active tab");
        let mid_view = harness.state().tabs[tab_idx].samples_len / 2;
        assert!(harness.state_mut().test_set_tab_view_offset(mid_view));
        harness.run_steps(1);
        let before = harness
            .state()
            .test_tab_view_offset()
            .expect("view offset before");
        for _ in 0..12 {
            editor_small_middle_drag_pan(&mut harness, 3.0);
        }
        let after = harness
            .state()
            .test_tab_view_offset()
            .expect("view offset after");
        assert_ne!(
            after, before,
            "high zoom middle drag should accumulate exact pan"
        );
    }

    #[test]
    fn editor_shift_arrow_then_shift_click_reuses_anchor() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        harness.state_mut().test_audio_seek_to_sample(4_000);
        harness.run_steps(1);
        harness.key_press_modifiers(Modifiers::SHIFT, Key::ArrowRight);
        harness.run_steps(2);
        let anchor = harness
            .state()
            .test_tab_selection_anchor()
            .expect("selection anchor");
        editor_shift_click_at_frac(&mut harness, 0.80);
        let selection = harness.state().test_tab_selection().expect("selection");
        assert_eq!(selection.0, anchor, "shift+click should reuse saved anchor");
        assert!(
            selection.1 > selection.0,
            "shift+click should extend the existing anchor-based range"
        );
    }

    #[test]
    fn editor_high_zoom_ctrl_arrow_sample_step_does_not_stall() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        for _ in 0..10 {
            editor_zoom_in_once(&mut harness);
        }
        let len = harness.state().test_tab_samples_len().max(1);
        let start = len / 2;
        harness.state_mut().test_audio_seek_to_sample(start);
        harness.run_steps(2);

        let before = harness
            .state()
            .test_audio_play_pos_display()
            .expect("playhead display before");
        for _ in 0..12 {
            harness.key_press_modifiers(Modifiers::CTRL, Key::ArrowRight);
            harness.run_steps(1);
        }
        let after = harness
            .state()
            .test_audio_play_pos_display()
            .expect("playhead display after");
        assert!(
            after >= before.saturating_add(8),
            "ctrl+arrow sample stepping should continue advancing at high zoom: before={before} after={after}"
        );
    }

    #[test]
    fn editor_high_zoom_ctrl_arrow_sample_step_does_not_stall_in_exact_stream_mapping() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        let len = harness.state().test_tab_samples_len().max(1);
        assert!(harness
            .state_mut()
            .test_set_active_tab_loading_visual_len(len.saturating_mul(2)));
        assert!(harness
            .state_mut()
            .test_force_active_tab_exact_stream_transport(48_000));
        for _ in 0..10 {
            editor_zoom_in_once(&mut harness);
        }
        harness.state_mut().test_audio_seek_to_sample(len / 2);
        harness.run_steps(2);

        let before = harness
            .state()
            .test_audio_play_pos_display()
            .expect("playhead display before");
        for _ in 0..12 {
            harness.key_press_modifiers(Modifiers::CTRL, Key::ArrowRight);
            harness.run_steps(1);
        }
        let after = harness
            .state()
            .test_audio_play_pos_display()
            .expect("playhead display after");
        assert!(
            after >= before.saturating_add(8),
            "ctrl+arrow should keep advancing under exact-stream display mapping: before={before} after={after}"
        );
    }

    #[test]
    fn editor_loading_visual_len_and_final_ready_keep_playhead_x_alignment() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        let len = harness.state().test_tab_samples_len().max(1);
        assert!(harness
            .state_mut()
            .test_set_active_tab_buffer_sample_rate(48_000));
        assert!(harness
            .state_mut()
            .test_set_active_tab_loading_visual_len(len.saturating_mul(2)));
        assert!(harness
            .state_mut()
            .test_force_active_tab_exact_stream_transport(44_100));
        harness.state_mut().test_set_mode_speed();
        harness.state_mut().test_set_playback_rate(1.0);
        harness
            .state_mut()
            .test_refresh_playback_mode_for_current_source(neowaves::app::RateMode::Speed, 1.0);

        let display_sr = harness
            .state()
            .test_active_editor_display_sample_rate()
            .expect("display sample rate");
        let target_display = (display_sr as usize).min(len.saturating_sub(1));
        assert!(harness
            .state_mut()
            .test_seek_active_editor_display_sample(target_display));
        harness.run_steps(2);

        let before_display = harness
            .state()
            .test_audio_play_pos_display()
            .expect("display before final ready");
        let before_x = harness
            .state()
            .test_editor_playhead_x_offset()
            .expect("playhead x before final ready");

        assert!(harness.state_mut().test_finish_active_tab_loading_visual());
        harness.run_steps(2);

        let after_display = harness
            .state()
            .test_audio_play_pos_display()
            .expect("display after final ready");
        let after_x = harness
            .state()
            .test_editor_playhead_x_offset()
            .expect("playhead x after final ready");
        assert!(
            after_display.abs_diff(before_display) <= 1,
            "final ready should not move display playhead: before={before_display} after={after_display}"
        );
        assert!(
            (after_x - before_x).abs() <= 0.51,
            "final ready should not move playhead x: before={before_x:.3} after={after_x:.3}"
        );
    }

    #[test]
    fn editor_max_zoom_playhead_x_matches_sample_center_and_roundtrips() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness
            .state_mut()
            .test_set_editor_pref_horizontal_zoom_anchor("playhead"));
        for _ in 0..12 {
            editor_zoom_in_once(&mut harness);
        }
        let (visible_start, visible_end) = harness
            .state()
            .test_editor_visible_display_range()
            .expect("visible range");
        let target = ((visible_start + visible_end) / 2).max(visible_start);
        assert!(harness
            .state_mut()
            .test_seek_active_editor_display_sample(target));
        harness.run_steps(2);

        let display = harness
            .state()
            .test_audio_play_pos_display()
            .expect("playhead display");
        let playhead_x = harness
            .state()
            .test_editor_playhead_x_offset()
            .expect("playhead x");
        let sample_x = harness
            .state()
            .test_editor_display_sample_x_offset(display)
            .expect("sample x");
        let roundtrip = harness
            .state()
            .test_editor_x_offset_to_display_sample(sample_x)
            .expect("sample roundtrip");
        assert!(
            (playhead_x - sample_x).abs() <= 0.01,
            "playhead x should sit on the same sample-center line: playhead={playhead_x:.4} sample={sample_x:.4}"
        );
        assert_eq!(
            roundtrip, display,
            "sample-center x should roundtrip to the same display sample: sample={display} roundtrip={roundtrip}"
        );
    }

    #[test]
    fn editor_zoom_in_out_keeps_playhead_sample_and_x_stable() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness
            .state_mut()
            .test_set_editor_pref_horizontal_zoom_anchor("playhead"));
        let len = harness
            .state()
            .test_editor_display_samples_len()
            .expect("display len")
            .max(2);
        let target = (len / 2).min(len.saturating_sub(2));
        assert!(harness
            .state_mut()
            .test_seek_active_editor_display_sample(target));
        harness.run_steps(2);

        let before_display = harness
            .state()
            .test_audio_play_pos_display()
            .expect("display before zoom");
        let before_x = harness
            .state()
            .test_editor_playhead_x_offset()
            .expect("x before zoom");
        editor_zoom_in_once(&mut harness);
        editor_zoom_out_once(&mut harness);
        let after_display = harness
            .state()
            .test_audio_play_pos_display()
            .expect("display after zoom");
        let after_x = harness
            .state()
            .test_editor_playhead_x_offset()
            .expect("x after zoom");
        assert!(
            after_display.abs_diff(before_display) <= 1,
            "zoom roundtrip should keep playhead sample stable: before={before_display} after={after_display}"
        );
        assert!(
            (after_x - before_x).abs() <= 0.51,
            "zoom roundtrip should keep playhead x stable: before={before_x:.3} after={after_x:.3}"
        );
    }

    #[test]
    fn editor_high_zoom_ctrl_arrow_reaches_edges_for_wav_mp3_m4a() {
        let dir = make_temp_dir("editor_step_formats");
        let fixtures = build_format_fixtures(&dir, 0.75);
        let mut harness = harness_with_folder(dir);
        wait_for_scan(&mut harness);

        for path in fixtures.into_iter().filter(|path| {
            path.extension()
                .and_then(|s| s.to_str())
                .map(|ext| matches!(ext, "wav" | "mp3" | "m4a"))
                .unwrap_or(false)
        }) {
            assert!(harness.state_mut().test_select_path(&path));
            harness.run_steps(2);
            ensure_editor_ready(&mut harness);
            let display_sr = harness
                .state()
                .test_active_editor_display_sample_rate()
                .expect("display sample rate");
            assert!(harness
                .state_mut()
                .test_force_active_tab_buffer_transport(display_sr));
            for _ in 0..12 {
                editor_zoom_in_once(&mut harness);
            }
            let len = harness
                .state()
                .test_editor_display_samples_len()
                .expect("display len")
                .max(2);
            harness
                .state_mut()
                .test_audio_seek_to_sample(len.saturating_sub(3));
            harness.run_steps(2);
            for _ in 0..8 {
                harness.key_press_modifiers(Modifiers::CTRL, Key::ArrowRight);
                harness.run_steps(1);
            }
            let at_right = harness
                .state()
                .test_audio_play_pos_display()
                .expect("display at right");
            assert_eq!(
                at_right.min(len.saturating_sub(1)),
                len.saturating_sub(1),
                "ctrl+arrow should reach the right edge for {}",
                path.display()
            );

            harness.state_mut().test_audio_seek_to_sample(2);
            harness.run_steps(2);
            for _ in 0..8 {
                harness.key_press_modifiers(Modifiers::CTRL, Key::ArrowLeft);
                harness.run_steps(1);
            }
            let at_left = harness
                .state()
                .test_audio_play_pos_display()
                .expect("display at left");
            assert_eq!(
                at_left,
                0,
                "ctrl+arrow should reach the left edge for {}",
                path.display()
            );
        }
    }

    #[test]
    fn editor_exact_stream_playhead_uses_editor_display_rate() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness
            .state_mut()
            .test_set_active_tab_buffer_sample_rate(48_000));
        assert!(harness
            .state_mut()
            .test_force_active_tab_exact_stream_transport(44_100));
        harness.state_mut().test_set_mode_speed();
        harness.state_mut().test_set_playback_rate(1.0);
        harness
            .state_mut()
            .test_refresh_playback_mode_for_current_source(neowaves::app::RateMode::Speed, 1.0);
        harness.state_mut().test_playback_seek_to_source_time(1.0);
        harness.run_steps(2);

        let display_sr = harness
            .state()
            .test_active_editor_display_sample_rate()
            .expect("display sample rate");
        let display_pos = harness
            .state()
            .test_audio_play_pos_display()
            .expect("display playhead");
        assert!(
            display_pos.abs_diff(display_sr as usize) <= 1,
            "editor playhead should use display sample rate, not transport sr: pos={display_pos} display_sr={display_sr}"
        );
    }

    #[test]
    fn editor_display_seek_roundtrip_preserves_source_time_in_exact_stream() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness
            .state_mut()
            .test_set_active_tab_buffer_sample_rate(48_000));
        assert!(harness
            .state_mut()
            .test_force_active_tab_exact_stream_transport(44_100));
        harness.state_mut().test_set_mode_speed();
        harness.state_mut().test_set_playback_rate(1.0);
        harness
            .state_mut()
            .test_refresh_playback_mode_for_current_source(neowaves::app::RateMode::Speed, 1.0);

        let display_sr = harness
            .state()
            .test_active_editor_display_sample_rate()
            .expect("display sample rate");
        let target_display = (display_sr as usize).saturating_mul(3) / 2;
        assert!(harness
            .state_mut()
            .test_seek_active_editor_display_sample(target_display));
        harness.run_steps(2);

        let source_time = harness
            .state()
            .test_playback_current_source_time_sec()
            .expect("source time");
        let display_after = harness
            .state()
            .test_audio_play_pos_display()
            .expect("display after");
        let expected_time = target_display as f64 / display_sr.max(1) as f64;
        assert!(
            (source_time - expected_time).abs() < 0.02,
            "display seek should preserve source time: expected={expected_time:.6} actual={source_time:.6}"
        );
        assert!(
            display_after.abs_diff(target_display) <= 1,
            "display seek should roundtrip through audio position: target={target_display} actual={display_after}"
        );
    }

    #[test]
    fn editor_buffer_speed_mode_playhead_tracks_source_time() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness
            .state_mut()
            .test_set_active_tab_buffer_sample_rate(48_000));
        assert!(harness
            .state_mut()
            .test_force_active_tab_buffer_transport(48_000));
        harness.state_mut().test_set_mode_speed();
        harness.state_mut().test_set_playback_rate(0.5);
        harness
            .state_mut()
            .test_refresh_playback_mode_for_current_source(neowaves::app::RateMode::Speed, 1.0);
        harness.state_mut().test_playback_seek_to_source_time(1.0);
        harness.run_steps(2);

        let display_sr = harness
            .state()
            .test_active_editor_display_sample_rate()
            .expect("display sample rate");
        let source_time = harness
            .state()
            .test_playback_current_source_time_sec()
            .expect("source time");
        let display_pos = harness
            .state()
            .test_audio_play_pos_display()
            .expect("display playhead");
        assert!(
            (source_time - 1.0).abs() < 0.02,
            "buffer speed mode should still track source time: {source_time:.6}"
        );
        assert!(
            display_pos.abs_diff(display_sr as usize) <= 1,
            "display playhead should stay on the audible source-time position under speed mode: pos={display_pos} display_sr={display_sr}"
        );
    }

    #[test]
    fn editor_loading_visual_len_and_final_ready_keep_playhead_alignment() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        let tab_len = harness.state().test_tab_samples_len().max(1);
        assert!(harness
            .state_mut()
            .test_set_active_tab_buffer_sample_rate(48_000));
        assert!(harness
            .state_mut()
            .test_set_active_tab_loading_visual_len(tab_len.saturating_mul(2)));
        assert!(harness
            .state_mut()
            .test_force_active_tab_exact_stream_transport(44_100));
        harness.state_mut().test_set_mode_speed();
        harness.state_mut().test_set_playback_rate(1.0);
        harness
            .state_mut()
            .test_refresh_playback_mode_for_current_source(neowaves::app::RateMode::Speed, 1.0);

        let display_sr = harness
            .state()
            .test_active_editor_display_sample_rate()
            .expect("display sample rate");
        let target_display = (display_sr as usize).min(tab_len.saturating_sub(1));
        assert!(harness
            .state_mut()
            .test_seek_active_editor_display_sample(target_display));
        harness.run_steps(2);
        let before_time = harness
            .state()
            .test_playback_current_source_time_sec()
            .expect("source time before final ready");
        let before_display = harness
            .state()
            .test_audio_play_pos_display()
            .expect("display before final ready");

        assert!(harness.state_mut().test_finish_active_tab_loading_visual());
        harness.run_steps(2);

        let after_time = harness
            .state()
            .test_playback_current_source_time_sec()
            .expect("source time after final ready");
        let after_display = harness
            .state()
            .test_audio_play_pos_display()
            .expect("display after final ready");
        assert!(
            (after_time - before_time).abs() < 0.02,
            "final ready should not move source time: before={before_time:.6} after={after_time:.6}"
        );
        assert!(
            after_display.abs_diff(before_display) <= 1,
            "final ready should not move display playhead: before={before_display} after={after_display}"
        );
    }

    #[test]
    fn editor_right_drag_then_shift_click_reuses_anchor() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        harness.state_mut().test_audio_seek_to_sample(2_000);
        harness.run_steps(1);
        editor_shift_right_drag(&mut harness, 0.30, 0.45);
        let anchor = harness
            .state()
            .test_tab_selection_anchor()
            .expect("selection anchor");
        editor_shift_click_at_frac(&mut harness, 0.80);
        let selection = harness.state().test_tab_selection().expect("selection");
        assert_eq!(
            selection.0, anchor,
            "shift+click should keep right-drag anchor"
        );
        assert!(
            selection.1 > selection.0,
            "shift+click should extend from the original right-drag anchor"
        );
    }

    #[test]
    fn editor_secondary_selection_anchor_is_button_down_sample() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        harness.state_mut().test_audio_seek_to_sample(1_200);
        harness.run_steps(1);
        editor_shift_right_drag(&mut harness, 0.65, 0.80);
        let anchor = harness
            .state()
            .test_tab_selection_anchor()
            .expect("selection anchor");
        let selection = harness.state().test_tab_selection().expect("selection");
        assert!(
            anchor > 20_000,
            "secondary selection anchor should come from button-down sample, not playhead: anchor={anchor}"
        );
        assert_eq!(selection.0, anchor);
    }

    #[test]
    fn editor_shift_right_drag_start_snaps_to_playhead_within_radius() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        let display_len = harness
            .state()
            .test_editor_display_samples_len()
            .expect("display len")
            .max(2);
        let playhead = display_len / 2;
        assert!(harness
            .state_mut()
            .test_seek_active_editor_display_sample(playhead));
        harness.run_steps(2);
        let playhead = harness
            .state()
            .test_audio_play_pos_display()
            .expect("actual playhead display");
        let playhead_x = harness
            .state()
            .test_editor_playhead_x_offset()
            .expect("playhead x");
        let wave_w = editor_wave_width(&harness);
        let start = editor_canvas_pos_at_x_offset(&harness, (playhead_x + 4.0).min(wave_w - 2.0));
        let end = editor_canvas_pos_at_x_offset(&harness, (playhead_x + 80.0).min(wave_w - 2.0));

        editor_shift_right_drag_between(&mut harness, start, end);

        let anchor = harness
            .state()
            .test_tab_selection_anchor()
            .expect("selection anchor");
        let selection = harness.state().test_tab_selection().expect("selection");
        assert_eq!(
            anchor, playhead,
            "shift+right drag should snap its start anchor to the playhead within 8px"
        );
        assert_eq!(selection.0, playhead);
        assert!(selection.1 > selection.0);
    }

    #[test]
    fn editor_shift_right_drag_start_outside_radius_uses_button_down_sample() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        let display_len = harness
            .state()
            .test_editor_display_samples_len()
            .expect("display len")
            .max(2);
        let playhead = display_len / 2;
        assert!(harness
            .state_mut()
            .test_seek_active_editor_display_sample(playhead));
        harness.run_steps(2);
        let playhead = harness
            .state()
            .test_audio_play_pos_display()
            .expect("actual playhead display");
        let playhead_x = harness
            .state()
            .test_editor_playhead_x_offset()
            .expect("playhead x");
        let wave_w = editor_wave_width(&harness);
        let start_x = (playhead_x + 20.0).min(wave_w - 2.0);
        let expected_anchor = harness
            .state()
            .test_editor_x_offset_to_display_sample(start_x)
            .expect("expected anchor");
        assert_ne!(
            expected_anchor, playhead,
            "test setup should place the button-down sample outside the snap radius"
        );
        let start = editor_canvas_pos_at_x_offset(&harness, start_x);
        let end = editor_canvas_pos_at_x_offset(&harness, (playhead_x + 100.0).min(wave_w - 2.0));

        editor_shift_right_drag_between(&mut harness, start, end);

        let anchor = harness
            .state()
            .test_tab_selection_anchor()
            .expect("selection anchor");
        assert_eq!(
            anchor, expected_anchor,
            "shift+right drag should preserve the button-down sample outside 8px"
        );
    }

    #[test]
    fn editor_shift_click_endpoint_snaps_to_playhead_within_radius() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        let display_len = harness
            .state()
            .test_editor_display_samples_len()
            .expect("display len")
            .max(2);
        let playhead = display_len / 2;
        assert!(harness.state_mut().test_set_selection_frac(0.10, 0.20));
        let anchor = harness
            .state()
            .test_tab_selection()
            .expect("initial selection")
            .0;
        assert!(harness
            .state_mut()
            .test_seek_active_editor_display_sample(playhead));
        harness.run_steps(2);
        let playhead = harness
            .state()
            .test_audio_play_pos_display()
            .expect("actual playhead display");
        let playhead_x = harness
            .state()
            .test_editor_playhead_x_offset()
            .expect("playhead x");
        let pos = editor_canvas_pos_at_x_offset(&harness, playhead_x + 4.0);

        editor_shift_click_at_pos(&mut harness, pos);

        let selection = harness.state().test_tab_selection().expect("selection");
        assert_eq!(selection.0, anchor, "shift+click should keep the existing anchor");
        assert_eq!(
            selection.1, playhead,
            "shift+click endpoint should snap to the playhead within 8px"
        );
    }

    #[test]
    fn editor_horizontal_zoom_anchor_pointer_keeps_pointer_sample() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        let tab_idx = harness.state().active_tab.expect("active tab");
        let len = harness.state().tabs[tab_idx].samples_len;
        harness.state_mut().test_audio_seek_to_sample(len / 4);
        assert!(harness
            .state_mut()
            .test_set_editor_pref_horizontal_zoom_anchor("pointer"));
        harness.run_steps(1);
        let before = editor_sample_at_ratio(&harness, 0.75);
        editor_zoom_in_at_frac(&mut harness, 0.75);
        let after = editor_sample_at_ratio(&harness, 0.75);
        assert!(
            after.abs_diff(before) <= 2_048,
            "pointer zoom anchor should keep the pointer sample stable: before={before} after={after}"
        );
    }

    #[test]
    fn editor_horizontal_zoom_anchor_playhead_keeps_playhead_sample() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        let tab_idx = harness.state().active_tab.expect("active tab");
        let len = harness.state().tabs[tab_idx].samples_len;
        harness.state_mut().test_audio_seek_to_sample(len / 4);
        assert!(harness
            .state_mut()
            .test_set_editor_pref_horizontal_zoom_anchor("playhead"));
        harness.run_steps(1);
        let before = editor_sample_at_ratio(&harness, 0.25);
        editor_zoom_in_at_frac(&mut harness, 0.75);
        let after = editor_sample_at_ratio(&harness, 0.25);
        assert!(
            after.abs_diff(before) <= 2_048,
            "playhead zoom anchor should keep the playhead sample stable: before={before} after={after}"
        );
    }

    #[test]
    fn editor_zoom_inversion_pref_roundtrip() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        let before = harness
            .state()
            .test_tab_samples_per_px()
            .expect("samples_per_px before");
        editor_zoom_in_once(&mut harness);
        let zoomed_in = harness
            .state()
            .test_tab_samples_per_px()
            .expect("samples_per_px zoomed in");
        assert!(zoomed_in < before);
        harness
            .state_mut()
            .test_set_editor_pref_invert_wave_zoom_wheel(true);
        editor_zoom_in_once(&mut harness);
        let inverted = harness
            .state()
            .test_tab_samples_per_px()
            .expect("samples_per_px inverted");
        assert!(
            inverted > zoomed_in,
            "inverted zoom wheel should reverse the zoom direction: zoomed_in={zoomed_in} inverted={inverted}"
        );
    }

    #[test]
    fn editor_shift_pan_inversion_pref_roundtrip() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        for _ in 0..8 {
            editor_zoom_in_once(&mut harness);
        }
        let tab_idx = harness.state().active_tab.expect("active tab");
        let base_view = harness.state().tabs[tab_idx].samples_len / 2;
        assert!(harness.state_mut().test_set_tab_view_offset(base_view));
        harness.run_steps(1);
        let before = harness
            .state()
            .test_tab_view_offset()
            .expect("view offset before");
        editor_shift_pan_once(&mut harness);
        let after_default = harness
            .state()
            .test_tab_view_offset()
            .expect("view offset default");
        assert!(harness.state_mut().test_set_tab_view_offset(base_view));
        harness
            .state_mut()
            .test_set_editor_pref_invert_shift_wheel_pan(true);
        harness.run_steps(1);
        editor_shift_pan_once(&mut harness);
        let after_inverted = harness
            .state()
            .test_tab_view_offset()
            .expect("view offset inverted");
        let delta_default = after_default as i64 - before as i64;
        let delta_inverted = after_inverted as i64 - base_view as i64;
        assert!(
            delta_default.signum() == -delta_inverted.signum(),
            "shift+wheel inversion should reverse pan direction: default={delta_default} inverted={delta_inverted}"
        );
    }

    #[test]
    fn editor_vertical_zoom_roundtrip_in_session() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        let dir = make_temp_dir("vertical_zoom_session");
        let sess = dir.join("vertical_zoom.nwsess");
        assert!(harness.state_mut().test_set_tab_vertical_zoom(3.2));
        assert!(harness.state_mut().test_set_tab_vertical_view_center(0.35));
        assert!(harness.state_mut().test_save_session_to(&sess));
        assert!(harness.state_mut().test_set_tab_vertical_zoom(1.0));
        assert!(harness.state_mut().test_set_tab_vertical_view_center(0.0));
        assert!(harness.state_mut().test_open_session_from(&sess));
        harness.run_steps(3);
        let zoom = harness
            .state()
            .test_tab_vertical_zoom()
            .expect("vertical zoom");
        let center = harness
            .state()
            .test_tab_vertical_view_center()
            .expect("vertical center");
        assert!(
            (zoom - 3.2).abs() < 0.01,
            "vertical zoom should roundtrip via session: {zoom}"
        );
        assert!(
            (center - 0.35).abs() < 0.02,
            "vertical center should roundtrip via session: {center}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn editor_vertical_view_center_roundtrip_in_session() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        let dir = make_temp_dir("vertical_center_session");
        let sess = dir.join("vertical_center.nwsess");
        assert!(harness.state_mut().test_set_tab_vertical_zoom(5.0));
        assert!(harness.state_mut().test_set_tab_vertical_view_center(-0.28));
        assert!(harness.state_mut().test_save_session_to(&sess));
        assert!(harness.state_mut().test_set_tab_vertical_zoom(1.0));
        assert!(harness.state_mut().test_set_tab_vertical_view_center(0.0));
        assert!(harness.state_mut().test_open_session_from(&sess));
        harness.run_steps(3);
        let center = harness
            .state()
            .test_tab_vertical_view_center()
            .expect("vertical center");
        assert!(
            (center + 0.28).abs() < 0.02,
            "vertical center should roundtrip via session: {center}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn editor_vertical_view_center_roundtrip_in_undo_redo() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness.state_mut().test_set_tab_vertical_zoom(4.0));
        assert!(harness.state_mut().test_set_tab_vertical_view_center(0.26));
        harness.run_steps(2);
        assert!(harness.state_mut().test_apply_reverse(0.1, 0.4));
        harness.run_steps(3);
        assert!(harness.state_mut().test_set_tab_vertical_zoom(1.0));
        assert!(harness.state_mut().test_set_tab_vertical_view_center(0.0));
        harness.run_steps(2);

        harness.key_press_modifiers(Modifiers::COMMAND, Key::Z);
        harness.run_steps(3);
        let undo_zoom = harness.state().test_tab_vertical_zoom().expect("undo zoom");
        let undo_center = harness
            .state()
            .test_tab_vertical_view_center()
            .expect("undo center");
        assert!(
            (undo_zoom - 4.0).abs() < 0.02 && (undo_center - 0.26).abs() < 0.02,
            "undo should restore vertical view state: zoom={undo_zoom} center={undo_center}"
        );

        harness.key_press_modifiers(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z);
        harness.run_steps(3);
        let redo_zoom = harness.state().test_tab_vertical_zoom().expect("redo zoom");
        let redo_center = harness
            .state()
            .test_tab_vertical_view_center()
            .expect("redo center");
        assert!(
            (redo_zoom - 4.0).abs() < 0.02 && (redo_center - 0.26).abs() < 0.02,
            "redo should restore the post-apply vertical view state: zoom={redo_zoom} center={redo_center}"
        );
    }

    #[test]
    fn editor_time_navigator_label_visible() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        let inspector_rect = harness.get_by_label("Inspector").rect();
        let label = editor_canvas_side_label(&harness, "Time");
        assert!(
            label.rect().right() < inspector_rect.left(),
            "Time label should live in the canvas area: {:?} vs {:?}",
            label.rect(),
            inspector_rect
        );
    }

    #[test]
    fn editor_amplitude_navigator_is_narrow_rail() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        harness.run_steps(2);
        let inspector_rect = harness.get_by_label("Inspector").rect();
        let rail_rect = editor_amplitude_nav_rect(&harness);
        assert!(
            rail_rect.right() < inspector_rect.left(),
            "Amplitude rail should live inside the canvas area: {:?} vs {:?}",
            rail_rect,
            inspector_rect
        );
        assert!(
            (rail_rect.width() - EDITOR_AMPLITUDE_NAV_STRIP_W).abs() <= 1.5,
            "Amplitude rail should be narrow: {:?}",
            rail_rect
        );
    }

    #[test]
    fn editor_amplitude_navigator_center_drag_changes_vertical_view_center() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness.state_mut().test_set_tab_vertical_zoom(4.0));
        harness.run_steps(2);
        let before_zoom = harness
            .state()
            .test_tab_vertical_zoom()
            .expect("vertical zoom before");
        let before_center = harness
            .state()
            .test_tab_vertical_view_center()
            .expect("vertical center before");
        editor_amplitude_nav_center_drag(&mut harness, 24.0);
        let after_zoom = harness
            .state()
            .test_tab_vertical_zoom()
            .expect("vertical zoom after");
        let after_center = harness
            .state()
            .test_tab_vertical_view_center()
            .expect("vertical center after");
        assert!(
            (after_zoom - before_zoom).abs() < 0.05,
            "center drag should keep zoom stable: before={before_zoom} after={after_zoom}"
        );
        assert!(
            (after_center - before_center).abs() > 0.05,
            "center drag should move vertical center: before={before_center} after={after_center}"
        );
    }

    #[test]
    fn editor_amplitude_navigator_edge_drag_changes_vertical_zoom() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness.state_mut().test_set_tab_vertical_zoom(2.0));
        harness.run_steps(2);
        let before = harness
            .state()
            .test_tab_vertical_zoom()
            .expect("vertical zoom before");
        editor_amplitude_nav_edge_drag(&mut harness, false, -24.0);
        let after = harness
            .state()
            .test_tab_vertical_zoom()
            .expect("vertical zoom after");
        assert!(
            after > before + 0.1,
            "Amplitude edge drag should zoom in: before={before} after={after}"
        );
    }

    #[test]
    fn editor_amplitude_navigator_edge_drag_keeps_working_outside_rail() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness.state_mut().test_set_tab_vertical_zoom(2.0));
        harness.run_steps(2);
        let before = harness
            .state()
            .test_tab_vertical_zoom()
            .expect("vertical zoom before");
        editor_amplitude_nav_edge_drag_outside_rail(&mut harness, false, 18.0, -24.0);
        let after = harness
            .state()
            .test_tab_vertical_zoom()
            .expect("vertical zoom after");
        assert!(
            after > before + 0.1,
            "Amplitude edge drag should keep working even when pointer leaves the narrow rail: before={before} after={after}"
        );
    }

    #[test]
    fn editor_amplitude_navigator_double_click_resets_zoom_and_center() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness.state_mut().test_set_tab_vertical_zoom(3.2));
        assert!(harness.state_mut().test_set_tab_vertical_view_center(0.30));
        harness.run_steps(2);
        editor_amplitude_nav_double_click(&mut harness);
        harness.run_steps(2);
        let zoom = harness
            .state()
            .test_tab_vertical_zoom()
            .expect("vertical zoom after reset");
        let center = harness
            .state()
            .test_tab_vertical_view_center()
            .expect("vertical center after reset");
        assert!(
            (zoom - 1.0).abs() < 0.01,
            "Amplitude rail double click should restore 1.0x zoom: {zoom}"
        );
        assert!(
            center.abs() < 0.01,
            "Amplitude rail double click should restore center to 0.0: {center}"
        );
    }

    #[test]
    fn editor_pause_resume_return_to_last_start() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness
            .state_mut()
            .test_set_editor_pref_pause_resume_mode("return_to_last_start"));
        harness.state_mut().test_audio_seek_to_sample(4_000);
        harness.run_steps(1);
        harness.key_press(Key::Space);
        harness.run_steps(3);
        assert_eq!(
            harness.state().test_last_play_start_display_sample(),
            Some(4_000)
        );
        harness.state_mut().test_audio_seek_to_sample(9_000);
        harness.run_steps(1);
        harness.key_press(Key::Space);
        harness.run_steps(3);
        assert!(!harness.state().test_audio_is_playing());
        assert_eq!(harness.state().test_audio_play_pos(), 4_000);
    }

    #[test]
    fn editor_pause_resume_continue_from_pause() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness
            .state_mut()
            .test_set_editor_pref_pause_resume_mode("continue_from_pause"));
        harness.state_mut().test_audio_seek_to_sample(4_000);
        harness.run_steps(1);
        harness.key_press(Key::Space);
        harness.run_steps(3);
        harness.state_mut().test_audio_seek_to_sample(9_000);
        harness.run_steps(1);
        harness.key_press(Key::Space);
        harness.run_steps(3);
        assert!(!harness.state().test_audio_is_playing());
        assert_eq!(harness.state().test_audio_play_pos(), 9_000);
    }

    #[test]
    fn editor_apply_gain_rebuilds_waveform_cache() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness.state().test_active_tab_waveform_pyramid_ready());
        assert!(harness.state_mut().test_apply_gain(0.2, 0.6, -6.0));
        harness.run_steps(1);
        assert!(harness.state().test_active_tab_waveform_pyramid_ready());
    }

    #[test]
    fn editor_apply_reverse_rebuilds_waveform_cache() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness.state().test_active_tab_waveform_pyramid_ready());
        assert!(harness.state_mut().test_apply_reverse(0.1, 0.4));
        harness.run_steps(1);
        assert!(harness.state().test_active_tab_waveform_pyramid_ready());
    }

    #[test]
    fn editor_apply_loop_unwrap_rebuilds_waveform_cache() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        let tab_idx = harness.state().active_tab.expect("active tab");
        let before_len = harness.state().tabs[tab_idx].samples_len;
        assert!(harness.state_mut().test_set_loop_region_frac(0.10, 0.20));
        assert!(harness.state_mut().test_apply_loop_unwrap(3));
        harness.run_steps(1);
        let after_len = harness.state().tabs[tab_idx].samples_len;
        assert!(after_len > before_len, "loop unwrap should extend the clip");
        assert!(harness.state().test_active_tab_waveform_pyramid_ready());
    }

    #[test]
    fn editor_stopped_meter_shows_neg_inf() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        harness.run_steps(2);
        assert!(
            harness.state().test_meter_db() <= -79.9,
            "stopped editor meter should report -inf-equivalent dBFS"
        );
        harness.state_mut().test_audio_seek_to_sample(10_000);
        harness.run_steps(1);
        harness.key_press(Key::Space);
        harness.run_steps(5);
        assert!(
            harness.state().test_meter_db() > -79.9,
            "playing editor meter should show real signal level"
        );
    }

    #[test]
    fn editor_waveform_overlay_in_spec_mode_survives_zoom_and_pan() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness
            .state_mut()
            .test_set_view_mode(neowaves::ViewMode::Spectrogram));
        assert!(harness.state_mut().test_set_waveform_overlay(true));
        harness.run_steps(3);

        editor_zoom_in_once(&mut harness);
        editor_shift_pan_once(&mut harness);

        let tab_idx = harness.state().active_tab.expect("active tab");
        assert_eq!(
            harness.state().tabs[tab_idx].leaf_view_mode(),
            neowaves::ViewMode::Spectrogram
        );
        assert!(harness.state().tabs[tab_idx].show_waveform_overlay);
        assert!(
            harness.state().test_active_tab_waveform_pyramid_ready(),
            "waveform cache should remain ready in spectrogram overlay mode"
        );
    }

    #[test]
    fn editor_channel_view_switch_all_custom_mixdown_keeps_waveform_visible() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness.state().test_active_tab_waveform_pyramid_ready());

        assert!(harness.state_mut().test_set_channel_view_all());
        harness.run_steps(3);
        assert!(harness.state().test_active_tab_waveform_pyramid_ready());

        assert!(harness.state_mut().test_set_channel_view_custom(vec![0]));
        harness.run_steps(3);
        assert!(harness.state().test_active_tab_waveform_pyramid_ready());

        assert!(harness.state_mut().test_set_channel_view_mixdown());
        harness.run_steps(3);
        assert!(harness.state().test_active_tab_waveform_pyramid_ready());
        assert!(
            harness.state().test_tab_samples_len() > 0,
            "waveform should remain renderable across channel view switches"
        );
    }

    #[test]
    fn editor_undo_redo_keeps_waveform_cache_renderable() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness.state().test_active_tab_waveform_pyramid_ready());

        assert!(harness.state_mut().test_apply_reverse(0.1, 0.4));
        harness.run_steps(3);
        assert!(harness.state().test_active_tab_waveform_pyramid_ready());

        harness.key_press_modifiers(Modifiers::COMMAND, Key::Z);
        harness.run_steps(3);
        assert!(
            harness.state().test_active_tab_waveform_pyramid_ready(),
            "undo should keep waveform cache renderable"
        );

        harness.key_press_modifiers(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z);
        harness.run_steps(3);
        assert!(
            harness.state().test_active_tab_waveform_pyramid_ready(),
            "redo should keep waveform cache renderable"
        );
    }

    #[test]
    fn editor_waveform_lod_counters_cover_raw_visible_and_pyramid() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        harness.run_steps(3);

        let (_, _, pyramid_before) = harness.state().test_waveform_lod_counts();
        harness.run_steps(2);
        let (_, _, pyramid_after) = harness.state().test_waveform_lod_counts();
        assert!(
            pyramid_after > pyramid_before,
            "fit-whole editor view should use pyramid LOD"
        );

        let visible_before = harness.state().test_waveform_lod_counts().1;
        for _ in 0..24 {
            editor_zoom_in_once(&mut harness);
            harness.run_steps(1);
            if harness.state().test_waveform_lod_counts().1 > visible_before {
                break;
            }
        }
        harness.run_steps(2);
        let visible_after = harness.state().test_waveform_lod_counts().1;
        assert!(
            visible_after > visible_before,
            "mid zoom should use visible-range min/max LOD"
        );

        let raw_before = harness.state().test_waveform_lod_counts().0;
        for _ in 0..32 {
            editor_zoom_in_once(&mut harness);
            harness.run_steps(1);
            if harness.state().test_waveform_lod_counts().0 > raw_before {
                break;
            }
        }
        harness.run_steps(2);
        let raw_after = harness.state().test_waveform_lod_counts().0;
        assert!(raw_after > raw_before, "deep zoom should use raw LOD");

        let summary = harness.state().test_debug_summary_text();
        assert!(summary.contains("waveform_render_ms:"));
        assert!(summary.contains("waveform_query_ms:"));
        assert!(summary.contains("waveform_draw_ms:"));
        assert!(summary.contains("waveform_lod_counts:"));
    }

    #[test]
    fn trim_set_add_virtual_keeps_editor_waveform_playback_source() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);

        let tab_idx = harness.state().active_tab.expect("active tab");
        let source_path = harness.state().tabs[tab_idx].path.clone();
        let source_len = harness.state().tabs[tab_idx].samples_len;
        let virtual_before = harness.state().test_virtual_item_count();

        assert!(harness.state_mut().test_set_active_tool(ToolKind::Trim));
        assert!(harness.state_mut().test_set_selection_frac(0.20, 0.60));
        harness.run_steps(2);

        harness.get_by_label("Set").click();
        harness.run_steps(2);
        assert_eq!(
            harness.state().tabs[tab_idx].preview_audio_tool,
            Some(ToolKind::Trim),
            "trim preview should be armed after Set"
        );

        harness.get_by_label("Add Trim As Virtual").click();
        harness.run_steps(1);
        assert!(
            harness.state().test_virtual_trim_active(),
            "Add Trim As Virtual should start asynchronously"
        );
        assert_eq!(
            harness.state().tabs[tab_idx].trim_range,
            None,
            "Add Trim As Virtual should clear consumed trim range"
        );
        wait_for_virtual_trim_done(&mut harness);
        assert_eq!(
            harness.state().test_virtual_item_count(),
            virtual_before + 1,
            "Add Trim As Virtual should create a new virtual item"
        );
        assert_eq!(
            harness.state().test_active_tab_path(),
            Some(source_path.clone()),
            "active editor tab should remain on source waveform"
        );
        assert_eq!(
            harness.state().tabs[tab_idx].preview_audio_tool,
            None,
            "trim preview should be cleared after creating virtual item"
        );

        harness.key_press(Key::Space);
        harness.run_steps(3);
        assert!(
            harness.state().test_audio_is_playing(),
            "space should start playback in editor"
        );
        assert_eq!(
            audio_buffer_len(harness.state()),
            source_len,
            "editor playback should use visible source waveform after Add Trim As Virtual"
        );
        assert_eq!(
            harness.state().test_playing_path().cloned(),
            Some(source_path),
            "playing path should remain source tab path"
        );
    }

    #[test]
    fn trim_v_shortcut_sets_selection_and_adds_virtual() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);

        let tab_idx = harness.state().active_tab.expect("active tab");
        let source_path = harness.state().tabs[tab_idx].path.clone();
        let virtual_before = harness.state().test_virtual_item_count();

        assert!(harness.state_mut().test_set_active_tool(ToolKind::Gain));
        assert!(harness.state_mut().test_set_selection_frac(0.20, 0.60));
        harness.run_steps(2);
        assert_eq!(
            harness.state().tabs[tab_idx].trim_range,
            None,
            "test setup should not pre-set the trim range"
        );

        harness.key_press(Key::V);
        harness.run_steps(1);
        assert!(
            harness.state().test_virtual_trim_active(),
            "V should start virtual trim creation asynchronously"
        );
        assert_eq!(
            harness.state().test_virtual_item_count(),
            virtual_before,
            "V should not create the virtual item synchronously"
        );
        assert_eq!(
            harness.state().tabs[tab_idx].selection,
            None,
            "V should clear selection like T after consuming the range"
        );
        assert_eq!(
            harness.state().tabs[tab_idx].trim_range,
            None,
            "V should not leave the Set trim range behind"
        );
        wait_for_virtual_trim_done(&mut harness);
        assert_eq!(
            harness.state().test_virtual_item_count(),
            virtual_before + 1,
            "V should create a virtual trim item"
        );
        assert_eq!(
            harness.state().test_active_tab_path(),
            Some(source_path),
            "V shortcut should keep the source editor tab active"
        );
        assert_eq!(
            harness.state().tabs[tab_idx].preview_audio_tool,
            None,
            "V shortcut should clear trim preview like the button path"
        );
        assert_eq!(
            harness.state().test_active_tool(),
            Some(ToolKind::Gain),
            "V should not force-switch the inspector tool"
        );
    }

    #[test]
    fn trim_v_shortcut_ignores_stale_trim_range_outside_trim_and_missing_range() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);

        assert!(harness.state_mut().test_set_active_tool(ToolKind::Gain));
        assert!(harness.state_mut().test_set_trim_range_frac(0.20, 0.60));
        let before_other_tool = harness.state().test_virtual_item_count();
        harness.key_press(Key::V);
        harness.run_steps(2);
        assert_eq!(
            harness.state().test_virtual_item_count(),
            before_other_tool,
            "V should not use a stale trim range outside the Trim tool when no selection is active"
        );

        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);

        assert!(harness.state_mut().test_set_active_tool(ToolKind::Trim));
        let before_missing_range = harness.state().test_virtual_item_count();
        harness.key_press(Key::V);
        harness.run_steps(2);
        assert_eq!(
            harness.state().test_virtual_item_count(),
            before_missing_range,
            "V should not create a virtual item without a trim range"
        );
    }

    #[test]
    fn trim_v_shortcut_uses_existing_trim_range_without_selection() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);

        assert!(harness.state_mut().test_set_active_tool(ToolKind::Trim));
        assert!(harness.state_mut().test_set_trim_range_frac(0.20, 0.60));
        let virtual_before = harness.state().test_virtual_item_count();
        harness.key_press(Key::V);
        harness.run_steps(1);
        assert!(harness.state().test_virtual_trim_active());
        assert_eq!(
            harness.state().tabs[harness.state().active_tab.expect("active tab")].trim_range,
            None,
            "V should clear existing Trim range after consuming it"
        );
        wait_for_virtual_trim_done(&mut harness);
        assert_eq!(
            harness.state().test_virtual_item_count(),
            virtual_before + 1,
            "V should use an existing trim range when no selection is active"
        );
    }

    #[test]
    fn clear_edits_on_virtual_item_after_v_shortcut_does_not_freeze_or_remove_it() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);

        assert!(harness.state_mut().test_set_active_tool(ToolKind::Gain));
        assert!(harness.state_mut().test_set_selection_frac(0.20, 0.60));
        let virtual_before = harness.state().test_virtual_item_count();
        harness.key_press(Key::V);
        harness.run_steps(1);
        assert!(harness.state().test_virtual_trim_active());
        wait_for_virtual_trim_done(&mut harness);
        assert_eq!(
            harness.state().test_virtual_item_count(),
            virtual_before + 1,
            "V should create exactly one virtual trim item"
        );
        let virtual_path = harness
            .state()
            .test_selected_path()
            .cloned()
            .expect("virtual item should be selected");

        assert!(harness
            .state_mut()
            .test_set_selected_sample_rate_override(22_050));
        assert!(harness.state().test_has_edits_for_selected());
        assert!(harness.state_mut().test_clear_selected_edits());
        harness.run_steps(2);

        assert_eq!(
            harness.state().test_virtual_item_count(),
            virtual_before + 1,
            "Clear Edits should clear virtual item overrides, not remove the virtual item"
        );
        assert_eq!(
            harness.state().test_selected_path(),
            Some(&virtual_path),
            "Clear Edits should keep the virtual item selected"
        );
        assert!(
            !harness.state().test_has_edits_for_selected(),
            "virtual item overrides should be cleared"
        );
    }

    #[test]
    fn clear_edits_for_cached_editor_payload_clears_without_snapshot_freeze() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);

        let source_path = harness
            .state()
            .test_active_tab_path()
            .expect("active source tab");
        assert!(harness.state_mut().test_apply_gain(0.20, 0.60, -6.0));
        assert!(harness.state().test_tab_dirty());
        assert!(harness.state_mut().test_close_active_tab());
        assert_eq!(
            harness.state().test_edited_cache_count(),
            1,
            "closing a dirty editor tab should create a cached edit"
        );

        assert!(harness.state_mut().test_select_path(&source_path));
        assert!(harness.state().test_has_edits_for_selected());
        assert!(harness.state_mut().test_clear_selected_edits());
        harness.run_steps(2);

        assert_eq!(
            harness.state().test_edited_cache_count(),
            0,
            "Clear Edits should remove the cached edit without taking a heavy undo snapshot"
        );
        assert!(
            !harness.state().test_has_edits_for_selected(),
            "source edits should be cleared"
        );
    }

    #[test]
    fn reopening_cached_editor_payload_uses_loading_placeholder_before_audio_ready() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);

        let source_path = harness
            .state()
            .test_active_tab_path()
            .expect("active source tab");
        assert!(harness.state_mut().test_apply_gain(0.20, 0.60, -6.0));
        assert!(harness.state_mut().test_close_active_tab());
        assert_eq!(harness.state().test_edited_cache_count(), 1);

        assert!(harness.state_mut().test_open_tab_for_path(&source_path));
        let tab_idx = harness.state().active_tab.expect("cached tab");
        assert_eq!(harness.state().tabs[tab_idx].path, source_path);
        assert!(
            harness.state().tabs[tab_idx].loading,
            "cached tab should show a loading placeholder immediately"
        );
        assert_eq!(
            harness.state().tabs[tab_idx].samples_len,
            0,
            "cached tab should not synchronously restore the full audio payload"
        );

        wait_for_tab_ready(&mut harness);
        let tab_idx = harness.state().active_tab.expect("cached tab ready");
        assert!(
            harness.state().tabs[tab_idx].samples_len > 0,
            "cached tab should receive audio after background restore"
        );
        assert_eq!(
            harness.state().test_active_tool(),
            Some(ToolKind::LoopEdit),
            "cached restore should preserve its own tool state"
        );
    }

    #[test]
    fn opening_virtual_item_uses_loading_placeholder_before_audio_ready() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);

        assert!(harness.state_mut().test_set_active_tool(ToolKind::Gain));
        assert!(harness.state_mut().test_set_selection_frac(0.20, 0.60));
        harness.key_press(Key::V);
        harness.run_steps(1);
        assert!(harness.state().test_virtual_trim_active());
        wait_for_virtual_trim_done(&mut harness);
        let virtual_path = harness
            .state()
            .test_selected_path()
            .cloned()
            .expect("virtual item should be selected");

        assert!(harness.state_mut().test_open_tab_for_path(&virtual_path));
        harness.run_steps(1);
        let tab_idx = harness.state().active_tab.expect("virtual tab");
        assert_eq!(harness.state().tabs[tab_idx].path, virtual_path);
        assert!(
            harness.state().tabs[tab_idx].loading,
            "virtual tab should show a loading placeholder immediately"
        );
        assert_eq!(
            harness.state().tabs[tab_idx].samples_len,
            0,
            "virtual tab should not synchronously clone audio into the editor"
        );

        wait_for_tab_ready(&mut harness);
        let tab_idx = harness.state().active_tab.expect("virtual tab ready");
        assert!(
            !harness.state().tabs[tab_idx].loading,
            "virtual tab should finish background open"
        );
        assert!(
            harness.state().tabs[tab_idx].samples_len > 0,
            "virtual tab should receive audio after background open"
        );
    }

    #[test]
    fn editor_gain_preview_restores_audio_and_overlay_in_wave() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);

        assert!(harness.state_mut().test_set_active_tool(ToolKind::Gain));
        assert!(harness.state_mut().test_set_tool_gain_db(6.0));
        assert!(harness.state_mut().test_refresh_tool_preview_active_tab());
        wait_for_preview_tool(&mut harness, ToolKind::Gain, true);

        assert_eq!(
            harness.state().test_preview_audio_tool(),
            Some(ToolKind::Gain)
        );
        assert_eq!(
            harness.state().test_preview_overlay_tool(),
            Some(ToolKind::Gain)
        );
        assert!(audio_buffer_len(harness.state()) > 0);
    }

    #[test]
    fn editor_normalize_preview_button_restores_overlay() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);

        assert!(harness
            .state_mut()
            .test_set_active_tool(ToolKind::Normalize));
        assert!(harness.state_mut().test_set_tool_normalize_target_db(-3.0));
        assert!(harness.state_mut().test_refresh_tool_preview_active_tab());
        wait_for_preview_tool(&mut harness, ToolKind::Normalize, true);

        assert_eq!(
            harness.state().test_preview_audio_tool(),
            Some(ToolKind::Normalize)
        );
        assert_eq!(
            harness.state().test_preview_overlay_tool(),
            Some(ToolKind::Normalize)
        );
    }

    #[test]
    fn editor_fade_preview_restores_overlay() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);

        assert!(harness.state_mut().test_set_active_tool(ToolKind::Fade));
        assert!(harness.state_mut().test_set_tool_fade_ms(120.0, 80.0));
        assert!(harness.state_mut().test_refresh_tool_preview_active_tab());
        wait_for_preview_tool(&mut harness, ToolKind::Fade, true);

        assert_eq!(
            harness.state().test_preview_audio_tool(),
            Some(ToolKind::Fade)
        );
        assert_eq!(
            harness.state().test_preview_overlay_tool(),
            Some(ToolKind::Fade)
        );
    }

    #[test]
    fn editor_preview_restore_survives_tab_switch() {
        let dir = make_temp_dir("preview_tab_switch");
        let a = dir.join("a.wav");
        let b = dir.join("b.wav");
        neowaves::wave::export_channels_audio(&synth_stereo(48_000, 2.0), 48_000, &a)
            .expect("export a");
        neowaves::wave::export_channels_audio(&synth_stereo(48_000, 1.5), 48_000, &b)
            .expect("export b");

        let mut harness = harness_with_folder(dir.clone());
        wait_for_scan(&mut harness);
        assert!(harness.state_mut().test_open_tab_for_path(&a));
        wait_for_tab_ready(&mut harness);
        assert!(harness.state_mut().test_set_active_tool(ToolKind::Gain));
        assert!(harness.state_mut().test_set_tool_gain_db(4.5));
        assert!(harness.state_mut().test_refresh_tool_preview_active_tab());
        wait_for_preview_tool(&mut harness, ToolKind::Gain, true);

        assert!(harness.state_mut().test_open_tab_for_path(&b));
        wait_for_tab_ready(&mut harness);
        assert_eq!(
            harness.state().test_active_tab_path().as_deref(),
            Some(b.as_path())
        );

        assert!(harness.state_mut().test_open_tab_for_path(&a));
        wait_for_tab_ready(&mut harness);
        wait_for_preview_tool(&mut harness, ToolKind::Gain, true);
        assert_eq!(
            harness.state().test_preview_overlay_tool(),
            Some(ToolKind::Gain)
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn editor_spec_overlay_mode_restores_preview_overlay() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);

        assert!(harness.state_mut().test_set_active_tool(ToolKind::Gain));
        assert!(harness.state_mut().test_set_tool_gain_db(5.0));
        assert!(harness
            .state_mut()
            .test_set_view_mode(neowaves::app::ViewMode::Spectrogram));
        assert!(harness.state_mut().test_set_waveform_overlay(true));
        assert!(harness.state_mut().test_refresh_tool_preview_active_tab());
        wait_for_preview_tool(&mut harness, ToolKind::Gain, true);

        assert_eq!(
            harness.state().test_preview_overlay_tool(),
            Some(ToolKind::Gain)
        );
        assert!(harness.state().test_preview_overlay_present());
    }

    #[test]
    fn editor_pitchshift_preview_result_stays_bound_to_origin_tab() {
        let dir = make_temp_dir("pitch_preview_restore");
        let a = dir.join("pitch_a.wav");
        let b = dir.join("pitch_b.wav");
        neowaves::wave::export_channels_audio(&synth_stereo(48_000, 2.8), 48_000, &a)
            .expect("export pitch_a");
        neowaves::wave::export_channels_audio(&synth_stereo(48_000, 1.4), 48_000, &b)
            .expect("export pitch_b");

        let mut harness = harness_with_folder(dir.clone());
        wait_for_scan(&mut harness);
        assert!(harness.state_mut().test_open_tab_for_path(&a));
        wait_for_tab_ready(&mut harness);
        assert!(harness
            .state_mut()
            .test_set_active_tool(ToolKind::PitchShift));
        assert!(harness.state_mut().test_set_tool_pitch_semitones(3.5));
        assert!(harness.state_mut().test_refresh_tool_preview_active_tab());
        harness.run_steps(2);

        assert!(harness.state_mut().test_open_tab_for_path(&b));
        wait_for_tab_ready(&mut harness);
        assert_eq!(
            harness.state().test_active_tab_path().as_deref(),
            Some(b.as_path())
        );

        assert!(harness.state_mut().test_open_tab_for_path(&a));
        wait_for_tab_ready(&mut harness);
        wait_for_preview_tool(&mut harness, ToolKind::PitchShift, true);
        wait_for_preview_idle(&mut harness);

        assert_eq!(
            harness.state().test_active_tab_path().as_deref(),
            Some(a.as_path())
        );
        assert_eq!(
            harness.state().test_preview_audio_tool(),
            Some(ToolKind::PitchShift)
        );
        assert_eq!(
            harness.state().test_preview_overlay_tool(),
            Some(ToolKind::PitchShift)
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn editor_timestretch_preview_result_stays_bound_to_origin_tab() {
        let dir = make_temp_dir("stretch_preview_restore");
        let a = dir.join("stretch_a.wav");
        let b = dir.join("stretch_b.wav");
        neowaves::wave::export_channels_audio(&synth_stereo(48_000, 2.6), 48_000, &a)
            .expect("export stretch_a");
        neowaves::wave::export_channels_audio(&synth_stereo(48_000, 1.2), 48_000, &b)
            .expect("export stretch_b");

        let mut harness = harness_with_folder(dir.clone());
        wait_for_scan(&mut harness);
        assert!(harness.state_mut().test_open_tab_for_path(&a));
        wait_for_tab_ready(&mut harness);
        assert!(harness
            .state_mut()
            .test_set_active_tool(ToolKind::TimeStretch));
        assert!(harness.state_mut().test_set_tool_stretch_rate(1.35));
        assert!(harness.state_mut().test_refresh_tool_preview_active_tab());
        harness.run_steps(2);

        assert!(harness.state_mut().test_open_tab_for_path(&b));
        wait_for_tab_ready(&mut harness);
        assert_eq!(
            harness.state().test_active_tab_path().as_deref(),
            Some(b.as_path())
        );

        assert!(harness.state_mut().test_open_tab_for_path(&a));
        wait_for_tab_ready(&mut harness);
        wait_for_preview_tool(&mut harness, ToolKind::TimeStretch, true);
        wait_for_preview_idle(&mut harness);

        assert_eq!(
            harness.state().test_active_tab_path().as_deref(),
            Some(a.as_path())
        );
        assert_eq!(
            harness.state().test_preview_audio_tool(),
            Some(ToolKind::TimeStretch)
        );
        assert_eq!(
            harness.state().test_preview_overlay_tool(),
            Some(ToolKind::TimeStretch)
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn topbar_playing_indicator_tracks_playback_state() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        select_first_row(&mut harness);
        harness.run_steps(2);
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(5) {
            harness.run_steps(1);
            if harness.state().test_audio_has_samples() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            harness.state().test_audio_has_samples(),
            "selected list item should have an audio source before manual play"
        );
        assert!(
            harness
                .query_all_by_label("Playing")
                .collect::<Vec<_>>()
                .is_empty(),
            "Playing indicator should be hidden while stopped"
        );

        harness.state_mut().audio.play();
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(5) {
            harness.run_steps(1);
            if harness.state().test_audio_is_playing() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(harness.state().test_audio_is_playing());
        assert!(
            !harness
                .query_all_by_label("Playing")
                .collect::<Vec<_>>()
                .is_empty(),
            "Playing indicator should be visible while playing"
        );

        harness.state_mut().audio.stop();
        harness.run_steps(3);
        assert!(
            harness
                .query_all_by_label("Playing")
                .collect::<Vec<_>>()
                .is_empty(),
            "Playing indicator should hide after stop"
        );
    }

    #[test]
    fn list_context_effect_graph_open_sets_target_path() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        let path = select_first_row(&mut harness);
        let label = path
            .file_name()
            .and_then(|s| s.to_str())
            .expect("file name")
            .to_string();

        harness.get_by_label(&label).click_secondary();
        harness.run_steps(2);
        harness.get_by_label("Effect Graph ⏵").click();
        harness.run_steps(1);
        harness.get_by_label("Open").click();
        harness.run_steps(3);

        assert!(harness.state().test_effect_graph_workspace_open());
        assert_eq!(harness.state().test_effect_graph_target_path(), Some(path));
    }

    #[test]
    fn effect_graph_plugin_node_controls_visible() {
        let mut harness = harness_empty();
        harness.state_mut().test_open_effect_graph_workspace();
        harness.run_steps(3);

        harness.get_by_label("Plugin FX");
        assert!(harness.state_mut().test_add_effect_graph_plugin_node());
        harness.run_steps(3);

        harness.get_by_label("Rescan");
        harness.get_by_label("Reload Params");
        harness.get_by_label("Enable");
        harness.get_by_label("Bypass");
    }

    #[test]
    fn effect_graph_duplicate_split_predicts_five_channels_and_shows_downmix_note() {
        let mut harness = harness_empty();
        harness
            .state_mut()
            .test_seed_effect_graph_duplicate_split_five_channel_doc();
        harness.run_steps(3);

        let summary = harness
            .state_mut()
            .test_effect_graph_predicted_output_summary()
            .expect("predicted summary");
        assert!(
            summary.contains("Predicted: 5 ch /"),
            "expected 5ch summary, got {summary}"
        );
        assert!(
            summary.ends_with("/ adaptive"),
            "expected adaptive summary, got {summary}"
        );
        assert!(
            !harness
                .query_all_by_label("Preview monitor downmixes >2ch to stereo")
                .collect::<Vec<_>>()
                .is_empty(),
            "expected monitor downmix note to be visible"
        );
    }

    #[test]
    fn spectrogram_hop_ui_shows_derived_overlap() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        harness.state_mut().test_set_show_export_settings(true);
        harness.state_mut().test_set_spectro_hop_size(128);
        harness.run_steps(3);

        harness.get_by_label("Hop Size:");
        harness.get_by_label("Overlap: 93.8% (derived)");
    }

    #[test]
    fn effect_graph_run_test_defers_pristine_input_decode() {
        let dir = wav_dir();
        let src = first_wav_file(&dir).expect("fixture wav");

        let mut harness = harness_with_folder(dir.clone());
        wait_for_scan(&mut harness);
        assert!(harness.state_mut().test_select_path(&src));
        harness.run_steps(2);
        harness
            .state_mut()
            .test_seed_effect_graph_duplicate_split_five_channel_doc();
        harness.run_steps(2);

        harness
            .state_mut()
            .test_start_effect_graph_test_run()
            .expect("start effect graph test run");

        assert!(
            harness.state().test_effect_graph_runner_active(),
            "expected runner to become active immediately"
        );
        assert!(
            !harness.state().test_effect_graph_last_input_audio_ready(),
            "pristine target should not decode input audio on the UI thread before worker results drain"
        );
        assert!(
            !harness.state().test_effect_graph_last_input_bus_ready(),
            "pristine target should not populate last_input_bus synchronously"
        );
    }

    #[test]
    fn settings_output_device_controls_visible() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        harness.state_mut().test_set_show_export_settings(true);
        harness.run_steps(3);

        harness.get_by_label("Audio Output:");
        harness.get_by_label("Refresh");
    }

    #[test]
    fn music_stem_preview_gain_clamps_to_plus_24_in_editor_ui() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness
            .state_mut()
            .test_set_active_tool(ToolKind::MusicAnalyze));
        assert!(harness
            .state_mut()
            .test_set_music_analysis_result_mock(true));
        assert!(harness
            .state_mut()
            .test_set_music_preview_gains_db(77.0, 33.0, 48.0, 60.0));
        harness.run_steps(3);

        let gains = harness
            .state()
            .test_music_preview_gains_db()
            .expect("music preview gains");
        assert!(gains.0 <= 24.0 && gains.0 >= -80.0);
        assert!(gains.1 <= 24.0 && gains.1 >= -80.0);
        assert!(gains.2 <= 24.0 && gains.2 >= -80.0);
        assert!(gains.3 <= 24.0 && gains.3 >= -80.0);
        assert!((gains.0 - 24.0).abs() < 1.0e-6);
    }

    #[test]
    fn music_analyze_ui_distinguishes_analysis_model_and_demucs_status() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        harness
            .state_mut()
            .test_set_mock_music_model_status(true, false);
        assert!(harness
            .state_mut()
            .test_set_active_tool(ToolKind::MusicAnalyze));
        harness.run_steps(3);

        harness.get_by_label("Analyze model: ready");
        harness.get_by_label("Auto Demucs: missing");
        harness.get_by_label("Repair Model Files...");
        harness.get_by_label("Input unavailable: stems not found and auto-Demucs is unavailable");
    }

    #[test]
    fn music_analyze_ui_shows_sonify_checkboxes() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness
            .state_mut()
            .test_set_active_tool(ToolKind::MusicAnalyze));
        assert!(harness
            .state_mut()
            .test_set_music_analysis_result_mock(true));
        harness.run_steps(3);

        harness.get_by_label("Beat Click");
        harness.get_by_label("DownBeat Accent");
        harness.get_by_label("Section Cue");
        harness.get_by_label("Apply writes the current stem mix and enabled cue sounds.");
    }

    #[test]
    fn music_analyze_sonify_checkbox_builds_preview_audio_and_overlay() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness
            .state_mut()
            .test_set_active_tool(ToolKind::MusicAnalyze));
        let source_len = harness.state().test_tab_samples_len().max(1);
        assert!(harness.state_mut().test_set_music_analysis_result_data(
            vec![source_len / 4],
            vec![source_len / 2],
            vec![(source_len * 3 / 4, "chorus".to_string())],
            source_len,
        ));
        assert!(harness.state_mut().test_set_mock_music_stems_audio(0.0));
        assert!(harness
            .state_mut()
            .test_set_music_sonify_flags(true, false, false));
        assert!(harness
            .state_mut()
            .test_apply_music_preview_mix_active_tab());

        wait_for_preview_tool(&mut harness, ToolKind::MusicAnalyze, true);
        wait_for_preview_idle(&mut harness);

        assert!(
            harness
                .state()
                .test_music_preview_peak_abs()
                .unwrap_or_default()
                > 0.0
        );
        assert_eq!(
            harness.state().test_preview_audio_tool(),
            Some(ToolKind::MusicAnalyze)
        );
        assert_eq!(
            harness.state().test_preview_overlay_tool(),
            Some(ToolKind::MusicAnalyze)
        );
    }

    #[test]
    fn model_download_progress_labels_show_n_over_n() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        harness
            .state_mut()
            .test_set_mock_transcript_model_download_progress(3, 7);
        harness
            .state_mut()
            .test_set_mock_music_model_download_progress(5, 9);
        harness.run_steps(2);

        harness.get_by_label("Downloading transcript model... 3/7");
        harness.get_by_label("Downloading music analyze model... 5/9");
        harness
            .state_mut()
            .test_clear_mock_model_download_progress();
    }

    #[cfg(feature = "kittest_render")]
    #[test]
    fn kittest_render_saves_editor_screenshot_png() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        harness.run_steps(2);

        let image = harness
            .render()
            .expect("kittest render should produce an image");
        assert!(image.width() >= 640);
        assert!(image.height() >= 360);

        let dir = make_temp_dir("kittest_render_shot");
        let out = dir.join("editor_kittest_render.png");
        image
            .save(&out)
            .unwrap_or_else(|e| panic!("save kittest render png failed: {e}"));
        let size = std::fs::metadata(&out).expect("png metadata").len();
        assert!(size > 1024, "rendered png looks too small: {size} bytes");
    }

    #[cfg(feature = "kittest_render")]
    #[test]
    fn kittest_render_zoom_ctrl_wheel_saves_before_after_screenshots() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        harness.run_steps(2);

        let before = harness
            .render()
            .expect("kittest render should produce pre-zoom image");
        let inspector_rect = harness.get_by_label("Inspector").rect();
        let hover_pos = egui::pos2(
            (inspector_rect.left() - 220.0).max(40.0),
            inspector_rect.center().y,
        );
        harness.hover_at(hover_pos);
        harness.event_modifiers(
            egui::Event::MouseWheel {
                unit: MouseWheelUnit::Point,
                delta: egui::vec2(0.0, 120.0),
                modifiers: Modifiers::COMMAND,
            },
            Modifiers::COMMAND,
        );
        harness.run_steps(3);
        let after = harness
            .render()
            .expect("kittest render should produce post-zoom image");
        assert_eq!(before.width(), after.width());
        assert_eq!(before.height(), after.height());

        let changed_pixels = before
            .pixels()
            .zip(after.pixels())
            .filter(|(a, b)| a.0 != b.0)
            .count();
        assert!(
            changed_pixels > 1024,
            "zoom render difference too small: {changed_pixels} changed pixels"
        );

        let dir = make_temp_dir("kittest_zoom_ctrl_wheel");
        let before_out = dir.join("zoom_before.png");
        let after_out = dir.join("zoom_after.png");
        before
            .save(&before_out)
            .unwrap_or_else(|e| panic!("save pre-zoom png failed: {e}"));
        after
            .save(&after_out)
            .unwrap_or_else(|e| panic!("save post-zoom png failed: {e}"));
        assert!(std::fs::metadata(&before_out).is_ok());
        assert!(std::fs::metadata(&after_out).is_ok());
    }

    #[cfg(feature = "kittest_render")]
    #[test]
    fn kittest_render_pan_changes_waveform_position_png() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        editor_zoom_in_once(&mut harness);
        harness.run_steps(2);

        let before = harness.render().expect("pre-pan render");
        editor_shift_pan_once(&mut harness);
        let after = harness.render().expect("post-pan render");

        let changed_pixels = before
            .pixels()
            .zip(after.pixels())
            .filter(|(a, b)| a.0 != b.0)
            .count();
        assert!(
            changed_pixels > 1024,
            "pan diff too small: {changed_pixels}"
        );

        let dir = make_temp_dir("kittest_pan_shift_wheel");
        let before_out = dir.join("pan_before.png");
        let after_out = dir.join("pan_after.png");
        before.save(&before_out).expect("save pan before");
        after.save(&after_out).expect("save pan after");
        assert!(std::fs::metadata(&before_out).is_ok());
        assert!(std::fs::metadata(&after_out).is_ok());
    }

    #[cfg(feature = "kittest_render")]
    #[test]
    fn kittest_render_horizontal_wheel_pan_changes_waveform_position_png() {
        let mut harness = harness_with_dynamic_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        for _ in 0..4 {
            editor_zoom_in_once(&mut harness);
        }
        harness.run_steps(2);

        let tab_idx = harness.state().active_tab.expect("active tab");
        let before_offset = harness.state().tabs[tab_idx].view_offset_exact;
        let before = harness.render().expect("pre-horizontal-pan render");

        let inspector_rect = harness.get_by_label("Inspector").rect();
        let hover_pos = egui::pos2(
            (inspector_rect.left() - 220.0).max(40.0),
            inspector_rect.center().y,
        );
        harness.hover_at(hover_pos);
        harness.event_modifiers(
            egui::Event::MouseWheel {
                unit: MouseWheelUnit::Point,
                delta: egui::vec2(180.0, 0.0),
                modifiers: Modifiers::NONE,
            },
            Modifiers::NONE,
        );
        harness.run_steps(3);

        let after_offset = harness.state().tabs[tab_idx].view_offset_exact;
        assert!(
            (after_offset - before_offset).abs() > 0.5,
            "horizontal wheel should pan the editor view: before={before_offset} after={after_offset}"
        );

        let after = harness.render().expect("post-horizontal-pan render");
        let changed_pixels = before
            .pixels()
            .zip(after.pixels())
            .filter(|(a, b)| a.0 != b.0)
            .count();
        assert!(
            changed_pixels > 1024,
            "horizontal pan render difference too small: {changed_pixels}"
        );

        let dir = make_temp_dir("kittest_horizontal_wheel_pan");
        let before_out = dir.join("horizontal_pan_before.png");
        let after_out = dir.join("horizontal_pan_after.png");
        before
            .save(&before_out)
            .expect("save horizontal pan before");
        after.save(&after_out).expect("save horizontal pan after");
        assert!(std::fs::metadata(&before_out).is_ok());
        assert!(std::fs::metadata(&after_out).is_ok());
    }

    #[cfg(feature = "kittest_render")]
    #[test]
    fn kittest_render_editor_resize_refit_saves_before_after_screenshots() {
        let mut harness = harness_with_dynamic_editor_fixture();
        harness.set_size(egui::vec2(900.0, 720.0));
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        harness.run_steps(3);
        assert_editor_whole_fit(&harness, "render before resize");
        let before = harness.render().expect("pre-resize render");

        harness.set_size(egui::vec2(1600.0, 720.0));
        harness.run_steps(6);
        assert_editor_whole_fit(&harness, "render after resize");
        let after = harness.render().expect("post-resize render");
        assert!(
            after.width() > before.width(),
            "post-resize screenshot should be wider: before={} after={}",
            before.width(),
            after.width()
        );

        let dir = make_temp_dir("kittest_editor_resize_refit");
        let before_out = dir.join("resize_fit_before.png");
        let after_out = dir.join("resize_fit_after.png");
        before.save(&before_out).expect("save resize before");
        after.save(&after_out).expect("save resize after");
        assert!(std::fs::metadata(&before_out).is_ok());
        assert!(std::fs::metadata(&after_out).is_ok());
    }

    #[cfg(feature = "kittest_render")]
    #[test]
    fn kittest_render_channel_view_all_vs_mixdown_differs_png() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        harness.run_steps(2);
        assert!(harness.state().test_active_tab_waveform_pyramid_ready());

        let mixdown = harness.render().expect("mixdown render");
        assert!(harness.state_mut().test_set_channel_view_all());
        harness.run_steps(3);
        let all = harness.render().expect("all-channels render");

        let changed_pixels = mixdown
            .pixels()
            .zip(all.pixels())
            .filter(|(a, b)| a.0 != b.0)
            .count();
        assert!(
            changed_pixels > 2048,
            "channel view render difference too small: {changed_pixels}"
        );

        let dir = make_temp_dir("kittest_channel_view_modes");
        let mixdown_out = dir.join("mixdown.png");
        let all_out = dir.join("all_channels.png");
        mixdown.save(&mixdown_out).expect("save mixdown");
        all.save(&all_out).expect("save all");
        assert!(std::fs::metadata(&mixdown_out).is_ok());
        assert!(std::fs::metadata(&all_out).is_ok());
    }

    #[cfg(feature = "kittest_render")]
    #[test]
    fn kittest_render_waveform_overlay_spec_zoom_png() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness
            .state_mut()
            .test_set_view_mode(neowaves::ViewMode::Spectrogram));
        assert!(harness.state_mut().test_set_waveform_overlay(true));
        harness.run_steps(3);

        let before = harness.render().expect("spec overlay pre-zoom render");
        editor_zoom_in_once(&mut harness);
        editor_shift_pan_once(&mut harness);
        let after = harness.render().expect("spec overlay post-zoom render");

        let changed_pixels = before
            .pixels()
            .zip(after.pixels())
            .filter(|(a, b)| a.0 != b.0)
            .count();
        assert!(
            changed_pixels > 1024,
            "spec overlay zoom/pan diff too small: {changed_pixels}"
        );

        let dir = make_temp_dir("kittest_spec_overlay_zoom");
        let before_out = dir.join("spec_overlay_before.png");
        let after_out = dir.join("spec_overlay_after.png");
        before.save(&before_out).expect("save spec overlay before");
        after.save(&after_out).expect("save spec overlay after");
        assert!(std::fs::metadata(&before_out).is_ok());
        assert!(std::fs::metadata(&after_out).is_ok());
    }
}
