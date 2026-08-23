#[cfg(feature = "kittest")]
mod kittest_suite {
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Mutex, MutexGuard, OnceLock};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use egui::{Key, Modifiers, MouseWheelUnit};
    use egui_kittest::{
        kittest::{NodeT, Queryable},
        Harness,
    };
    use neowaves::app::{ColumnId, ColumnKey};
    use neowaves::app::{EditorNote, EditorNotePositionMode, ToolKind};
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

    fn now_millis() -> u128 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
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

    fn synth_auto_trim_sections_stereo(sr: u32) -> Vec<Vec<f32>> {
        let secs = 3.0_f32;
        let frames = (sr as f32 * secs) as usize;
        let mut left = vec![0.0_f32; frames];
        let mut right = vec![0.0_f32; frames];
        let mut write_burst = |start_sec: f32, dur_sec: f32, freq: f32, amp: f32| {
            let start = (start_sec * sr as f32) as usize;
            let end = ((start_sec + dur_sec) * sr as f32) as usize;
            for i in start.min(frames)..end.min(frames) {
                let t = i as f32 / sr as f32;
                left[i] = (t * freq * std::f32::consts::TAU).sin() * amp;
                right[i] = (t * (freq * 1.5) * std::f32::consts::TAU).sin() * (amp * 0.8);
            }
        };
        write_burst(0.50, 0.35, 440.0, 0.45);
        write_burst(1.35, 0.40, 660.0, 0.40);
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

    fn harness_with_long_editor_fixture() -> Harness<'static, WavesPreviewer> {
        let dir = make_temp_dir("long_editor_fixture");
        let sr = 48_000;
        // Keep this just above LIVE_PREVIEW_SAMPLE_LIMIT so the simplified
        // long-clip preview path is exercised without a needlessly large file.
        let chans = synth_stereo(sr, 42.0);
        let path = dir.join("long_editor_fixture.wav");
        neowaves::wave::export_channels_audio(&chans, sr, &path)
            .unwrap_or_else(|e| panic!("export long editor fixture failed: {e}"));
        harness_with_folder(dir)
    }

    fn harness_with_auto_trim_sections_fixture() -> Harness<'static, WavesPreviewer> {
        let dir = make_temp_dir("auto_trim_sections_fixture");
        let sr = 48_000;
        let chans = synth_auto_trim_sections_stereo(sr);
        let path = dir.join("auto_trim_sections_fixture.wav");
        neowaves::wave::export_channels_audio(&chans, sr, &path)
            .unwrap_or_else(|e| panic!("export auto trim sections fixture failed: {e}"));
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

    fn audio_buffer_peak(state: &WavesPreviewer) -> f32 {
        state
            .audio
            .shared
            .samples
            .load_full()
            .map(|buffer| {
                buffer
                    .channels
                    .iter()
                    .flat_map(|channel| channel.iter())
                    .fold(0.0_f32, |peak, &sample| peak.max(sample.abs()))
            })
            .unwrap_or(0.0)
    }

    fn active_tab_peak(state: &WavesPreviewer) -> f32 {
        let Some(tab_idx) = state.active_tab else {
            return 0.0;
        };
        state.tabs[tab_idx]
            .ch_samples
            .iter()
            .flat_map(|channel| channel.iter())
            .fold(0.0_f32, |peak, &sample| peak.max(sample.abs()))
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

    /// Find a list row whose duration is known and long enough to seek within.
    ///
    /// `wait_for_scan` only waits for the *listing*; durations arrive later
    /// from the metadata pool, which is slower when the suite runs in parallel.
    /// Metadata is queued for visible rows first, so the first screenful
    /// resolves; pump frames until one does.
    fn wait_for_seekable_row(
        harness: &mut Harness<'static, WavesPreviewer>,
        min_secs: f64,
        ext: Option<&str>,
    ) -> usize {
        let start = Instant::now();
        loop {
            let found = (0..harness.state().files.len()).find(|&r| {
                let ext_ok = ext.is_none_or(|want| {
                    path_for_row(harness.state(), r)
                        .extension()
                        .and_then(|e| e.to_str())
                        == Some(want)
                });
                ext_ok
                    && harness
                        .state()
                        .test_row_duration_secs(r)
                        .is_some_and(|d| d > min_secs)
            });
            if let Some(row) = found {
                return row;
            }
            assert!(
                start.elapsed() < SCAN_TIMEOUT,
                "no row reached a known duration > {min_secs}s (ext={ext:?})"
            );
            harness.run_steps(1);
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

    fn wait_for_tab_fully_loaded(harness: &mut Harness<'static, WavesPreviewer>) {
        let start = Instant::now();
        loop {
            harness.run_steps(1);
            if let Some(idx) = harness.state().active_tab {
                if harness
                    .state()
                    .tabs
                    .get(idx)
                    .is_some_and(|tab| !tab.loading && tab.samples_len > 0)
                {
                    break;
                }
            }
            if start.elapsed() > TAB_READY_TIMEOUT {
                panic!("full tab decode timeout");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// WORLD jobs are CPU-heavy and the Rust test runner otherwise starts all
    /// three GUI analysis cases concurrently. Serialize them so this timeout
    /// measures one job rather than thread-pool contention.
    const WORLD_ANALYSIS_TIMEOUT: Duration = Duration::from_secs(90);

    fn world_analysis_test_guard() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    fn wait_for_world_features(
        harness: &mut Harness<'static, WavesPreviewer>,
    ) -> (usize, usize, f32) {
        let start = Instant::now();
        loop {
            harness.run_steps(1);
            if let Some(features) = harness.state().test_world_features_ready() {
                return features;
            }
            if start.elapsed() > WORLD_ANALYSIS_TIMEOUT {
                panic!("WORLD analysis timeout");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn wait_for_project_path(harness: &mut Harness<'static, WavesPreviewer>, path: &Path) {
        let expected = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let start = Instant::now();
        loop {
            harness.run_steps(1);
            let current = harness
                .state()
                .test_project_path()
                .and_then(|p| std::fs::canonicalize(&p).ok().or(Some(p)));
            if current.as_ref() == Some(&expected) {
                break;
            }
            if start.elapsed() > Duration::from_secs(10) {
                panic!(
                    "project open timeout: expected {} current {:?}",
                    expected.display(),
                    current
                );
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
                phase: egui::TouchPhase::Move,
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
                phase: egui::TouchPhase::Move,
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
                phase: egui::TouchPhase::Move,
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
                phase: egui::TouchPhase::Move,
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
                phase: egui::TouchPhase::Move,
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

    fn editor_wave_lane_rect(
        harness: &Harness<'static, WavesPreviewer>,
        lane_index: usize,
        lane_count: usize,
    ) -> egui::Rect {
        let nav = harness
            .state()
            .test_tab_amplitude_nav_rect()
            .expect("amplitude navigator for Pencil lane");
        let top = nav.top() - 10.0;
        let bottom = nav.bottom() + 10.0;
        let lane_h = (bottom - top) / lane_count.max(1) as f32;
        egui::Rect::from_min_max(
            egui::pos2(editor_wave_left(harness), top + lane_h * lane_index as f32),
            egui::pos2(
                editor_wave_left(harness) + editor_wave_width(harness),
                top + lane_h * (lane_index + 1) as f32,
            ),
        )
    }

    fn editor_pencil_point_pos(
        harness: &Harness<'static, WavesPreviewer>,
        sample: usize,
        channel: usize,
        lane_index: usize,
        lane_count: usize,
    ) -> egui::Pos2 {
        let tab_idx = harness.state().active_tab.expect("active Pencil tab");
        let tab = &harness.state().tabs[tab_idx];
        let lane = editor_wave_lane_rect(harness, lane_index, lane_count);
        let overlay = tab
            .preview_overlay
            .as_ref()
            .expect("Pencil preview overlay");
        let amp = overlay.channels[channel][sample];
        let gain_scale = 10.0f32.powf(harness.state().test_pending_gain_db(&tab.path) / 20.0);
        let zoom = tab.vertical_zoom.max(1.0);
        let visible_half = 1.0 / zoom;
        let center_limit = (1.0 - visible_half).max(0.0);
        let center = if zoom <= 1.0 {
            0.0
        } else {
            tab.vertical_view_center.clamp(-center_limit, center_limit)
        };
        let normalized =
            (((amp * gain_scale).clamp(-1.0, 1.0) - center) / visible_half).clamp(-1.0, 1.0);
        let x_offset = harness
            .state()
            .test_editor_display_sample_x_offset(sample)
            .expect("Pencil sample x offset");
        egui::pos2(
            editor_wave_left(harness) + x_offset,
            lane.center().y - normalized * lane.height() * 0.48,
        )
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
                phase: egui::TouchPhase::Move,
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

    fn pointer_drag_with_press_frame(
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
        harness.run_steps(1);
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

    fn editor_pointer_drag_with_modifiers(
        harness: &mut Harness<'static, WavesPreviewer>,
        start: egui::Pos2,
        end: egui::Pos2,
        modifiers: Modifiers,
    ) {
        harness.hover_at(start);
        harness.event_modifiers(
            egui::Event::PointerButton {
                pos: start,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers,
            },
            modifiers,
        );
        harness.event_modifiers(egui::Event::PointerMoved(end), modifiers);
        harness.run_steps(2);
        harness.event_modifiers(
            egui::Event::PointerButton {
                pos: end,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers,
            },
            modifiers,
        );
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

    fn editor_primary_click_at_pos(
        harness: &mut Harness<'static, WavesPreviewer>,
        pos: egui::Pos2,
    ) {
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

    fn rightmost_labeled_control<'a>(
        harness: &'a Harness<'static, WavesPreviewer>,
        label: &'a str,
    ) -> egui_kittest::Node<'a> {
        harness
            .query_all_by_label(label)
            .max_by(|a, b| {
                a.rect()
                    .center()
                    .x
                    .partial_cmp(&b.rect().center().x)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .unwrap_or_else(|| panic!("rightmost control '{label}' not found"))
    }

    fn first_label_rect(harness: &Harness<'static, WavesPreviewer>, label: &str) -> egui::Rect {
        harness
            .query_all_by_label(label)
            .min_by(|a, b| {
                a.rect()
                    .min
                    .y
                    .partial_cmp(&b.rect().min.y)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .unwrap_or_else(|| panic!("label '{label}' not found"))
            .rect()
    }

    fn list_columns_row_label_center(
        harness: &Harness<'static, WavesPreviewer>,
        label: &str,
    ) -> egui::Pos2 {
        let drag_label = format!("Drag {label} column");
        let row = harness.get_by_label(&drag_label).rect();
        harness
            .query_all_by_label(label)
            .find(|node| node.rect().intersects(row))
            .unwrap_or_else(|| panic!("List Columns row label '{label}' not found"))
            .rect()
            .center()
    }

    fn assert_rect_nearly_same(a: egui::Rect, b: egui::Rect, label: &str) {
        let tolerance = 2.0;
        assert!(
            (a.left() - b.left()).abs() <= tolerance
                && (a.top() - b.top()).abs() <= tolerance
                && (a.width() - b.width()).abs() <= tolerance
                && (a.height() - b.height()).abs() <= tolerance,
            "{label} moved/resized too much: before={a:?} after={b:?}"
        );
    }

    #[test]
    fn kittest_list_columns_window_moves_and_rows_drag_from_their_labels() {
        let mut harness = harness_with_wavs(false);
        harness.set_size(egui::vec2(1600.0, 900.0));
        wait_for_scan(&mut harness);
        harness
            .state_mut()
            .test_add_metadata_list_column("ucs.cat_id", "UCS Category");
        harness.run_steps(3);

        top_menu_button(&harness, "Tools").click();
        harness.run_steps(1);
        harness.get_by_label("List Columns...").click();
        harness.run_steps(5);
        let reset = harness
            .query_all_by_label("Reset")
            .max_by(|a, b| {
                a.rect()
                    .center()
                    .x
                    .partial_cmp(&b.rect().center().x)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .expect("List Columns Reset button");
        reset.click();
        harness.run_steps(2);

        let before_pos = harness
            .state()
            .test_list_columns_window_pos()
            .expect("initial List Columns position");
        let title_start = egui::pos2(before_pos.x + 260.0, before_pos.y + 12.0);
        let title_delta = egui::vec2(150.0, -55.0);
        pointer_drag_with_press_frame(&mut harness, title_start, title_start + title_delta);
        let moved_pos = harness
            .state()
            .test_list_columns_window_pos()
            .expect("moved List Columns position");
        let actual_delta = moved_pos - before_pos;
        assert!(
            (actual_delta.x - title_delta.x).abs() <= 4.0
                && (actual_delta.y - title_delta.y).abs() <= 4.0,
            "title drag should move the window by the pointer delta: before={before_pos:?} start={title_start:?} expected={title_delta:?} actual={actual_delta:?}"
        );
        assert_eq!(
            harness.state().test_list_columns_window_global_pos(),
            Some(moved_pos),
            "a no-Session title drag should update the global persisted position"
        );
        harness
            .query_all_by_label("Reset")
            .max_by(|a, b| {
                a.rect()
                    .center()
                    .x
                    .partial_cmp(&b.rect().center().x)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .expect("List Columns Reset after move")
            .click();
        harness.run_steps(2);
        assert_eq!(
            harness.state().test_list_columns_window_pos(),
            Some(moved_pos),
            "Reset should affect column settings but not the window position"
        );

        let file_start = list_columns_row_label_center(&harness, "File");
        let edited_drop = list_columns_row_label_center(&harness, "Edited");
        editor_pointer_drag(&mut harness, file_start, edited_drop);
        assert_eq!(
            harness.state().list_column_layout[0],
            ColumnKey::Builtin(ColumnId::File),
            "the Built-in label itself should be a drag source"
        );

        assert!(
            !harness.state().list_columns.cover_art,
            "Art must remain hidden before the hidden-row drag"
        );
        let art_start = list_columns_row_label_center(&harness, "Art");
        let file_drop = list_columns_row_label_center(&harness, "File");
        editor_pointer_drag(&mut harness, art_start, file_drop);
        assert_eq!(
            harness.state().list_column_layout[0],
            ColumnKey::Builtin(ColumnId::CoverArt),
            "a hidden Built-in row should be reorderable"
        );
        assert!(!harness.state().list_columns.cover_art);

        let external_start = list_columns_row_label_center(&harness, "External");
        let art_drop = list_columns_row_label_center(&harness, "Art");
        editor_pointer_drag(&mut harness, external_start, art_drop);
        assert_eq!(
            harness.state().list_column_layout[0],
            ColumnKey::Builtin(ColumnId::External),
            "an unavailable External row should still be reorderable"
        );

        harness.get_by_label("Show Art column").click();
        harness.run_steps(2);
        assert!(
            harness.state().list_columns.cover_art,
            "clicking a checkbox must still toggle visibility"
        );
        assert_eq!(
            harness.state().list_column_layout[1],
            ColumnKey::Builtin(ColumnId::CoverArt),
            "a checkbox click must not start a row drag"
        );

        let scroll_hover = list_columns_row_label_center(&harness, "Bitrate");
        harness.hover_at(scroll_hover);
        for _ in 0..24 {
            harness.event(egui::Event::MouseWheel {
                unit: MouseWheelUnit::Line,
                delta: egui::vec2(0.0, -5.0),
                phase: egui::TouchPhase::Move,
                modifiers: Modifiers::NONE,
            });
            harness.run_steps(1);
        }
        let metadata_start = list_columns_row_label_center(&harness, "UCS Category");
        let note_drop = list_columns_row_label_center(&harness, "Note");
        editor_pointer_drag(&mut harness, metadata_start, note_drop);
        assert_eq!(
            harness.state().list_column_layout.last(),
            Some(&ColumnKey::Normalized("ucs.cat_id".to_string())),
            "Metadata should use the same row-label drag behavior as Built-in columns"
        );

        harness
            .state_mut()
            .test_set_list_columns_window_pos(Some(egui::pos2(-5_000.0, 5_000.0)));
        harness.run_steps(3);
        let constrained = harness
            .state()
            .test_list_columns_window_pos()
            .expect("constrained List Columns position");
        assert!(
            constrained.x >= 0.0
                && constrained.y >= 0.0
                && constrained.x < 1600.0
                && constrained.y < 900.0,
            "off-screen positions should be constrained into the viewport: {constrained:?}"
        );
    }

    #[test]
    fn kittest_list_columns_position_roundtrips_through_prefs_and_session() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        let dir = make_temp_dir("list_columns_position");
        let prefs = dir.join("prefs.txt");
        let session = dir.join("position.nwsess");

        let global_pos = egui::pos2(84.0, 96.0);
        harness
            .state_mut()
            .test_set_list_columns_window_global_pos(Some(global_pos));
        harness
            .state_mut()
            .test_set_list_columns_window_pos(Some(global_pos));
        harness.state().test_save_prefs_to_path(&prefs);
        harness
            .state_mut()
            .test_set_list_columns_window_global_pos(None);
        harness.state_mut().test_set_list_columns_window_pos(None);
        harness.state_mut().test_load_prefs_from_path(&prefs);
        assert_eq!(
            harness.state().test_list_columns_window_global_pos(),
            Some(global_pos)
        );
        assert_eq!(
            harness.state().test_list_columns_window_pos(),
            Some(global_pos),
            "a no-Session workflow should restore the global position"
        );

        let session_pos = egui::pos2(333.5, 144.25);
        harness
            .state_mut()
            .test_set_list_columns_window_pos(Some(session_pos));
        assert!(harness.state_mut().test_save_session_to(&session));
        harness
            .state_mut()
            .test_set_list_columns_window_global_pos(Some(global_pos));
        harness
            .state_mut()
            .test_set_list_columns_window_pos(Some(egui::pos2(1.0, 2.0)));
        assert!(harness.state_mut().test_open_session_from(&session));
        assert_eq!(
            harness.state().test_list_columns_window_pos(),
            Some(session_pos),
            "the Session-specific position should take precedence over prefs"
        );
        assert!(harness.state_mut().test_close_session_with_autosave());
        assert_eq!(
            harness.state().test_list_columns_window_pos(),
            Some(global_pos),
            "closing the Session should restore the global position"
        );
    }

    #[cfg(feature = "kittest_render")]
    #[test]
    fn kittest_render_list_columns_window_move_and_builtin_label_drag_evidence() {
        let mut harness = harness_with_wavs(false);
        harness.set_size(egui::vec2(1600.0, 900.0));
        wait_for_scan(&mut harness);

        top_menu_button(&harness, "Tools").click();
        harness.run_steps(1);
        harness.get_by_label("List Columns...").click();
        harness.run_steps(5);
        harness
            .query_all_by_label("Reset")
            .max_by(|a, b| {
                a.rect()
                    .center()
                    .x
                    .partial_cmp(&b.rect().center().x)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .expect("List Columns Reset button")
            .click();
        harness.run_steps(2);

        let out_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("debug")
            .join("screenshot_verify")
            .join("list_columns_drag");
        std::fs::create_dir_all(&out_dir).expect("create List Columns drag evidence dir");
        harness
            .render()
            .expect("render List Columns initial position")
            .save(out_dir.join("01_before.png"))
            .expect("save List Columns initial screenshot");

        let before_pos = harness
            .state()
            .test_list_columns_window_pos()
            .expect("initial window position");
        let title_start = egui::pos2(before_pos.x + 260.0, before_pos.y + 12.0);
        let title_delta = egui::vec2(170.0, -60.0);
        pointer_drag_with_press_frame(&mut harness, title_start, title_start + title_delta);
        let moved_pos = harness
            .state()
            .test_list_columns_window_pos()
            .expect("moved window position");
        let actual_delta = moved_pos - before_pos;
        assert!(
            (actual_delta.x - title_delta.x).abs() <= 4.0
                && (actual_delta.y - title_delta.y).abs() <= 4.0,
            "evidence drag should move the window: expected={title_delta:?} actual={actual_delta:?}"
        );
        harness
            .render()
            .expect("render moved List Columns window")
            .save(out_dir.join("02_window_moved.png"))
            .expect("save moved List Columns screenshot");

        let file_start = list_columns_row_label_center(&harness, "File");
        let edited_drop = list_columns_row_label_center(&harness, "Edited");
        harness.hover_at(file_start);
        harness.event(egui::Event::PointerButton {
            pos: file_start,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: Modifiers::NONE,
        });
        harness.event(egui::Event::PointerMoved(edited_drop));
        harness.run_steps(2);
        assert_eq!(
            harness.state().list_column_order[0],
            ColumnId::Edited,
            "order must not change before pointer release"
        );
        harness
            .render()
            .expect("render Built-in row while dragging")
            .save(out_dir.join("03_builtin_dragging.png"))
            .expect("save Built-in dragging screenshot");

        harness.event(egui::Event::PointerButton {
            pos: edited_drop,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        });
        harness.run_steps(3);
        assert_eq!(
            harness.state().list_column_order[0],
            ColumnId::File,
            "releasing the Built-in label on Edited should commit the new order"
        );
        harness
            .render()
            .expect("render reordered Built-in rows")
            .save(out_dir.join("04_builtin_reordered.png"))
            .expect("save reordered Built-in screenshot");
    }

    #[cfg(feature = "kittest_render")]
    fn assert_inspector_labels_inside(harness: &Harness<'static, WavesPreviewer>, labels: &[&str]) {
        let inspector = harness
            .state()
            .test_editor_inspector_rect()
            .expect("inspector rect");
        for label in labels {
            let nodes: Vec<_> = harness
                .query_all_by_label(*label)
                .filter(|node| node.rect().intersects(inspector.expand(2.0)))
                .collect();
            assert!(!nodes.is_empty(), "inspector label '{label}' not found");
            for node in nodes {
                let rect = node.rect();
                assert!(
                    rect.right() <= inspector.right() + 2.0,
                    "inspector label '{label}' overflows right edge: node={rect:?} inspector={inspector:?}"
                );
            }
        }
    }

    #[cfg(feature = "kittest_render")]
    fn strong_red_pixels_in_rect(image: &image::RgbaImage, rect: egui::Rect) -> usize {
        let width = image.width() as i32;
        let height = image.height() as i32;
        let left = rect.left().floor().max(0.0) as i32;
        let top = rect.top().floor().max(0.0) as i32;
        let right = rect.right().ceil().min(width as f32) as i32;
        let bottom = rect.bottom().ceil().min(height as f32) as i32;
        let mut count = 0usize;
        for y in top.max(0)..bottom.max(0).min(height) {
            for x in left.max(0)..right.max(0).min(width) {
                let p = image.get_pixel(x as u32, y as u32).0;
                if p[0] > 170 && p[1] < 105 && p[2] < 105 && p[0] > p[1].saturating_add(55) {
                    count += 1;
                }
            }
        }
        count
    }

    #[cfg(feature = "kittest_render")]
    fn ui_stability_screenshot_dir() -> PathBuf {
        let out_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("debug")
            .join("screenshot_verify")
            .join("ui_stability");
        std::fs::create_dir_all(&out_dir).expect("create screenshot verify dir");
        out_dir
    }

    #[cfg(feature = "kittest_render")]
    fn assert_topbar_volume_meter_has_no_red(
        harness: &Harness<'static, WavesPreviewer>,
        image: &image::RgbaImage,
        label: &str,
    ) {
        let volume = harness
            .state()
            .test_topbar_volume_rect()
            .unwrap_or_else(|| panic!("{label}: volume rect"));
        let meter = harness
            .state()
            .test_topbar_output_meter_rect()
            .unwrap_or_else(|| panic!("{label}: meter rect"));
        let red = strong_red_pixels_in_rect(image, volume.expand(2.0))
            + strong_red_pixels_in_rect(image, meter.expand(2.0));
        assert_eq!(
            red, 0,
            "{label} volume/meter area should not contain strong red pixels"
        );
    }

    #[cfg(feature = "kittest_render")]
    fn render_ui_stability_png(
        harness: &mut Harness<'static, WavesPreviewer>,
        file_name: &str,
    ) -> image::RgbaImage {
        harness.run_steps(2);
        let image = harness.render().expect("render image");
        assert_topbar_volume_meter_has_no_red(harness, &image, file_name);
        let out = ui_stability_screenshot_dir().join(file_name);
        image
            .save(&out)
            .unwrap_or_else(|e| panic!("save {} failed: {e}", out.display()));
        image
    }

    #[cfg(feature = "kittest_render")]
    #[test]
    fn kittest_render_external_dialog_many_columns_stays_visible_and_scrolls() {
        let fixture_dir = make_temp_dir("external_dialog_many_columns");
        let mut cfg = StartupConfig::default();
        cfg.external_dummy_rows = Some(20);
        cfg.external_dummy_cols = 180;
        cfg.external_dummy_path = Some(fixture_dir.join("many_columns.csv"));
        cfg.external_show_dialog = true;

        let mut harness = harness_with_startup(cfg);
        let viewport = egui::vec2(900.0, 620.0);
        harness.set_size(viewport);
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            harness.run_steps(1);
            if harness
                .query_all_by_label("Visible Columns")
                .next()
                .is_some()
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "external dummy table did not finish loading"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        harness.run_steps(3);

        let out_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("debug")
            .join("screenshot_verify")
            .join("external_dialog_scroll");
        std::fs::create_dir_all(&out_dir).expect("create external dialog screenshot dir");
        harness
            .render()
            .expect("render external dialog top")
            .save(out_dir.join("kittest_top.png"))
            .expect("save external dialog top screenshot");

        for label in [
            "External Data",
            "Load CSV/Excel...",
            "Visible Columns",
            "Col2",
        ] {
            let rect = first_label_rect(&harness, label);
            assert!(
                rect.left() >= 0.0
                    && rect.top() >= 0.0
                    && rect.right() <= viewport.x
                    && rect.bottom() <= viewport.y,
                "{label} must stay inside the viewport: {rect:?}"
            );
        }

        let column_hover = first_label_rect(&harness, "Col2").center();
        harness.hover_at(column_hover);
        for _ in 0..36 {
            harness.event(egui::Event::MouseWheel {
                unit: MouseWheelUnit::Line,
                delta: egui::vec2(0.0, -6.0),
                phase: egui::TouchPhase::Move,
                modifiers: Modifiers::default(),
            });
            harness.run_steps(1);
        }
        let last_column = first_label_rect(&harness, "Col180");
        assert!(
            last_column.bottom() <= viewport.y,
            "last external column should be reachable by scrolling: {last_column:?}"
        );
        harness
            .render()
            .expect("render external columns scrolled")
            .save(out_dir.join("kittest_columns_scrolled.png"))
            .expect("save external columns scrolled screenshot");

        let outer_hover = first_label_rect(&harness, "Scope (optional)").center();
        harness.hover_at(outer_hover);
        for _ in 0..12 {
            harness.event(egui::Event::MouseWheel {
                unit: MouseWheelUnit::Line,
                delta: egui::vec2(0.0, -5.0),
                phase: egui::TouchPhase::Move,
                modifiers: Modifiers::default(),
            });
            harness.run_steps(1);
        }
        let summary = first_label_rect(&harness, "Matched: 0  Unmatched: 0");
        assert!(
            summary.bottom() <= viewport.y,
            "dialog footer should be reachable by outer scrolling: {summary:?}"
        );
        harness
            .render()
            .expect("render external dialog bottom")
            .save(out_dir.join("kittest_dialog_scrolled.png"))
            .expect("save external dialog bottom screenshot");
    }

    #[cfg(feature = "kittest_render")]
    #[test]
    fn kittest_render_list_columns_window_toggles_and_reorders_columns() {
        let mut harness = harness_with_wavs(false);
        harness.set_size(egui::vec2(1600.0, 900.0));
        wait_for_scan(&mut harness);
        harness
            .state_mut()
            .test_add_metadata_list_column("ucs.cat_id", "UCS Category");
        harness.run_steps(3);

        top_menu_button(&harness, "Tools").click();
        harness.run_steps(1);
        harness.get_by_label("List Columns...").click();
        harness.run_steps(5);
        assert!(harness.state().test_show_list_columns_window());
        harness
            .query_all_by_label("Reset")
            .max_by(|a, b| {
                a.rect()
                    .center()
                    .x
                    .partial_cmp(&b.rect().center().x)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .expect("List Columns Reset button")
            .click();
        harness.run_steps(2);
        assert_eq!(
            harness.state().list_column_order[0],
            ColumnId::Edited,
            "fixture should begin with the default built-in order"
        );
        assert!(
            !harness.state().list_columns.cover_art,
            "Art should begin hidden"
        );

        let out_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("debug")
            .join("screenshot_verify")
            .join("list_columns");
        std::fs::create_dir_all(&out_dir).expect("create List Columns screenshot dir");
        harness
            .render()
            .expect("render List Columns before changes")
            .save(out_dir.join("01_before.png"))
            .expect("save List Columns before screenshot");

        let window_rect = first_label_rect(&harness, "List Columns");
        assert_eq!(
            harness.state().list_column_layout.last(),
            Some(&ColumnKey::Builtin(ColumnId::Note)),
            "Note should begin at the far-right end of the unified order"
        );
        assert!(
            harness
                .query_all_by_label("Show Note column")
                .next()
                .is_some(),
            "Note visibility must be controlled in the unified list"
        );
        let file_drag = harness.get_by_label("Drag File column").rect().center();
        let edited_drop = harness
            .query_all_by_label("Edited")
            .filter(|node| node.rect().intersects(window_rect))
            .max_by(|a, b| {
                a.rect()
                    .center()
                    .x
                    .partial_cmp(&b.rect().center().x)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .expect("Edited drop row")
            .rect()
            .center();
        editor_pointer_drag(&mut harness, file_drag, edited_drop);
        assert_eq!(
            harness.state().list_column_order[0],
            ColumnId::File,
            "dragging File onto Edited should move File to the left edge"
        );
        harness
            .query_all_by_label("Reset")
            .max_by(|a, b| {
                a.rect()
                    .center()
                    .x
                    .partial_cmp(&b.rect().center().x)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .expect("List Columns Reset button")
            .click();
        harness.run_steps(2);
        assert_eq!(harness.state().list_column_order[0], ColumnId::Edited);

        let list_hover = first_label_rect(&harness, "Bitrate").center();
        harness.hover_at(list_hover);
        for _ in 0..22 {
            harness.event(egui::Event::MouseWheel {
                unit: MouseWheelUnit::Line,
                delta: egui::vec2(0.0, -5.0),
                phase: egui::TouchPhase::Move,
                modifiers: Modifiers::NONE,
            });
            harness.run_steps(1);
        }
        let metadata_drag = harness
            .get_by_label("Drag UCS Category column")
            .rect()
            .center();
        let note_drop = harness.get_by_label("Drag Note column").rect().center();
        editor_pointer_drag(&mut harness, metadata_drag, note_drop);
        assert_eq!(
            harness.state().list_column_layout.last(),
            Some(&ColumnKey::Normalized("ucs.cat_id".to_string())),
            "metadata and built-in columns should share the same D&D order"
        );
        harness
            .query_all_by_label("Reset")
            .max_by(|a, b| {
                a.rect()
                    .center()
                    .x
                    .partial_cmp(&b.rect().center().x)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .expect("List Columns Reset button")
            .click();
        harness.run_steps(2);
        harness.hover_at(egui::pos2(760.0, 430.0));
        for _ in 0..22 {
            harness.event(egui::Event::MouseWheel {
                unit: MouseWheelUnit::Line,
                delta: egui::vec2(0.0, 5.0),
                phase: egui::TouchPhase::Move,
                modifiers: Modifiers::NONE,
            });
            harness.run_steps(1);
        }

        harness.get_by_label("Show Art column").click();
        harness.run_steps(2);
        assert!(
            harness.state().list_columns.cover_art,
            "Art checkbox should update the List immediately"
        );

        let art_drag = harness.get_by_label("Drag Art column").rect().center();
        let edited_drop = harness
            .query_all_by_label("Edited")
            .filter(|node| node.rect().intersects(window_rect))
            .max_by(|a, b| {
                a.rect()
                    .center()
                    .x
                    .partial_cmp(&b.rect().center().x)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .expect("Edited drop row after reset")
            .rect()
            .center();
        editor_pointer_drag(&mut harness, art_drag, edited_drop);
        assert_eq!(
            harness.state().list_column_layout[0],
            ColumnKey::Builtin(ColumnId::CoverArt),
            "moving Art earlier should place it at the left edge"
        );
        assert_eq!(
            harness.state().list_column_layout[1],
            ColumnKey::Builtin(ColumnId::Edited)
        );
        harness
            .render()
            .expect("render List Columns after changes")
            .save(out_dir.join("02_art_visible_and_first.png"))
            .expect("save List Columns changed screenshot");

        harness.state_mut().test_set_show_list_columns_window(false);
        for column in ColumnId::ALL {
            column.set_enabled(&mut harness.state_mut().list_columns, false);
        }
        ColumnId::File.set_enabled(&mut harness.state_mut().list_columns, true);
        ColumnId::Note.set_enabled(&mut harness.state_mut().list_columns, true);
        harness.state_mut().items[0].note = "Editable list note saved in the session".to_string();
        harness.run_steps(3);
        harness
            .render()
            .expect("render editable Note column")
            .save(out_dir.join("03_note_column.png"))
            .expect("save Note column screenshot");
        assert_eq!(
            harness.state().items[0].note,
            "Editable list note saved in the session"
        );
        assert!(
            harness.query_all_by_label("Note").next().is_some(),
            "the editable Note column should be visible in the List"
        );

        harness.state_mut().test_set_show_export_settings(true);
        harness.run_steps(3);
        assert!(
            harness.query_all_by_label("List Columns:").next().is_none(),
            "List Columns controls must no longer live inside Settings"
        );
        assert!(
            harness.query_all_by_label("Column Order").next().is_none(),
            "the old Settings order editor must be removed"
        );
    }

    #[cfg(feature = "kittest_render")]
    #[test]
    fn kittest_render_editor_notes_restore_time_and_frequency_selection() {
        let mut harness = harness_with_editor_fixture();
        harness.set_size(egui::vec2(1600.0, 900.0));
        ensure_editor_ready(&mut harness);
        assert!(harness
            .state_mut()
            .test_set_view_mode(neowaves::ViewMode::Spectrogram));
        assert!(harness
            .state_mut()
            .test_set_active_tool(ToolKind::EditorNote));
        let tab_idx = harness.state().active_tab.expect("active editor tab");
        let sr = harness.state().tabs[tab_idx].buffer_sample_rate.max(1) as usize;
        {
            let tab = &mut harness.state_mut().tabs[tab_idx];
            tab.bpm_enabled = true;
            tab.bpm_value = 120.0;
            tab.bpm_offset_sec = 0.0;
            tab.time_sig_numerator = 4;
            tab.time_sig_denominator = 4;
            tab.editor_note_position_mode = EditorNotePositionMode::Time;
            tab.selection = None;
            tab.freq_selection = None;
            tab.editor_notes = vec![
                EditorNote {
                    id: 1,
                    comment: "Transient begins here".to_string(),
                    start_sample: sr / 2,
                    end_sample: None,
                    freq_range_hz: None,
                    view: None,
                },
                EditorNote {
                    id: 2,
                    comment: "Spectral chorus band".to_string(),
                    start_sample: sr,
                    end_sample: Some(sr * 2),
                    freq_range_hz: Some((220.0, 2_400.0)),
                    view: Some("Spectrogram".to_string()),
                },
            ];
        }
        harness.run_steps(4);
        let inspector_hover = first_label_rect(&harness, "Transient begins here").center();
        harness.hover_at(inspector_hover);
        for _ in 0..5 {
            harness.event(egui::Event::MouseWheel {
                unit: MouseWheelUnit::Line,
                delta: egui::vec2(0.0, -4.0),
                phase: egui::TouchPhase::Move,
                modifiers: Modifiers::NONE,
            });
            harness.run_steps(1);
        }

        let out_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("debug")
            .join("screenshot_verify")
            .join("editor_notes");
        std::fs::create_dir_all(&out_dir).expect("create Editor Note screenshot dir");
        harness
            .render()
            .expect("render Editor Note list")
            .save(out_dir.join("01_editor_note_list.png"))
            .expect("save Editor Note list screenshot");
        assert!(
            harness
                .query_all_by_label("Spectral chorus band")
                .next()
                .is_some(),
            "Editor Note comment should be listed"
        );
        assert!(
            harness
                .query_all_by_label("Spectrogram · 220–2400 Hz")
                .next()
                .is_some(),
            "spectral notes should display view and frequency range"
        );

        let view_before = format!("{:?}", harness.state().tabs[tab_idx].leaf_view_mode());
        harness.get_by_label("0:01.0 – 0:02.0").click();
        harness.run_steps(1);
        assert_eq!(
            harness.state().tabs[tab_idx]
                .editor_note_last_click
                .map(|v| v.0),
            Some(2),
            "first location click should reach the Editor Note button"
        );
        harness.get_by_label("0:01.0 – 0:02.0").click();
        harness.run_steps(1);
        harness.run_steps(3);
        assert_eq!(harness.state().tabs[tab_idx].selection, Some((sr, sr * 2)));
        assert_eq!(
            harness.state().tabs[tab_idx].freq_selection,
            Some((220.0, 2_400.0))
        );
        assert_eq!(
            format!("{:?}", harness.state().tabs[tab_idx].leaf_view_mode()),
            view_before,
            "restoring a spectral Editor Note must keep the current editor view"
        );
        harness
            .render()
            .expect("render restored Editor Note selection")
            .save(out_dir.join("02_double_click_restored_selection.png"))
            .expect("save restored selection screenshot");

        let inspector_hover = first_label_rect(&harness, "Spectral chorus band").center();
        harness.hover_at(inspector_hover);
        for _ in 0..5 {
            harness.event(egui::Event::MouseWheel {
                unit: MouseWheelUnit::Line,
                delta: egui::vec2(0.0, 4.0),
                phase: egui::TouchPhase::Move,
                modifiers: Modifiers::NONE,
            });
            harness.run_steps(1);
        }
        harness.get_by_label("Beats").click();
        harness.run_steps(3);
        assert_eq!(
            harness.state().tabs[tab_idx].editor_note_position_mode,
            EditorNotePositionMode::Beats
        );
        assert!(
            harness
                .query_all_by_label("1:3.00 – 2:1.00")
                .next()
                .is_some(),
            "beat mode should display bar:beat positions"
        );
        harness.hover_at(egui::pos2(1420.0, 500.0));
        for _ in 0..5 {
            harness.event(egui::Event::MouseWheel {
                unit: MouseWheelUnit::Line,
                delta: egui::vec2(0.0, -4.0),
                phase: egui::TouchPhase::Move,
                modifiers: Modifiers::NONE,
            });
            harness.run_steps(1);
        }
        harness
            .render()
            .expect("render Editor Note beat positions")
            .save(out_dir.join("03_editor_note_beats.png"))
            .expect("save beat-position screenshot");
    }

    #[test]
    fn load_folder_shows_files() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        assert!(!harness.state().files.is_empty());
        assert!(harness.state().root.is_some());
    }

    #[test]
    fn external_drag_selected_multi_queues_selected_set() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        assert!(
            harness.state().files.len() >= 2,
            "fixture needs at least two audio files"
        );
        let paths = {
            let state = harness.state();
            vec![path_for_row(state, 0), path_for_row(state, 1)]
        };
        assert!(harness.state_mut().test_select_paths_multi(&paths));

        assert!(harness.state_mut().test_queue_external_drag_for_row(1));
        assert_eq!(harness.state().test_pending_external_drag_len(), 2);
        let prepared = harness
            .state_mut()
            .test_prepare_external_drag_paths_for_pending()
            .expect("prepare drag paths");

        assert_eq!(prepared.len(), 2);
        assert!(prepared.iter().all(|path| path.is_file()));
    }

    #[test]
    fn external_drag_unselected_row_queues_single_item() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        assert!(
            harness.state().files.len() >= 3,
            "fixture needs at least three audio files"
        );
        let selected = {
            let state = harness.state();
            vec![path_for_row(state, 0), path_for_row(state, 1)]
        };
        let target = {
            let state = harness.state();
            path_for_row(state, 2)
        };
        assert!(harness.state_mut().test_select_paths_multi(&selected));

        assert!(harness.state_mut().test_queue_external_drag_for_row(2));
        assert_eq!(harness.state().test_pending_external_drag_len(), 1);
        assert_eq!(harness.state().test_selected_path(), Some(&target));
        let prepared = harness
            .state_mut()
            .test_prepare_external_drag_paths_for_pending()
            .expect("prepare drag paths");

        assert_eq!(prepared.len(), 1);
        assert_eq!(
            prepared[0],
            std::fs::canonicalize(target).expect("canonical target")
        );
    }

    #[test]
    fn temp_cache_audio_files_are_hidden_from_list_scan() {
        let dir = make_temp_dir("temp_cache_hidden");
        let normal = dir.join("normal.wav");
        neowaves::wave::export_channels_audio(&[vec![0.0, 0.1, 0.0]], 48_000, &normal)
            .expect("write normal wav");
        let cache_dir = std::env::temp_dir().join("NeoWaves").join("drag");
        std::fs::create_dir_all(&cache_dir).expect("create cache dir");
        let cache = cache_dir.join(format!(
            "nwcache_kittest_{}_{}.wav",
            std::process::id(),
            now_millis()
        ));
        neowaves::wave::export_channels_audio(&[vec![0.0, 0.2, 0.0]], 48_000, &cache)
            .expect("write cache wav");
        let mut cfg = StartupConfig::default();
        cfg.open_files = vec![normal.clone(), cache.clone()];
        let mut harness = harness_with_startup(cfg);
        wait_for_scan(&mut harness);

        let listed_paths: Vec<PathBuf> = harness
            .state()
            .files
            .iter()
            .filter_map(|id| {
                harness
                    .state()
                    .item_index
                    .get(id)
                    .and_then(|idx| harness.state().items.get(*idx))
                    .map(|item| item.path.clone())
            })
            .collect();

        assert!(listed_paths.contains(&normal));
        assert!(
            !listed_paths.contains(&cache),
            "internal temp cache should not appear in List"
        );
        let _ = std::fs::remove_file(cache);
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

    /// Recursively scan a [`egui::Shape`] tree for the debug overlays egui paints when
    /// `Context::check_for_id_clash` (🔥 debug text) or `warn_if_rect_changes_id`
    /// (plain red rect outline, no text) fire.
    fn collect_id_clash_shapes(shape: &egui::Shape, hits: &mut Vec<(String, egui::Rect)>) {
        match shape {
            egui::Shape::Vec(shapes) => {
                for s in shapes {
                    collect_id_clash_shapes(s, hits);
                }
            }
            egui::Shape::Text(text_shape) => {
                let text = &text_shape.galley.job.text;
                if text.contains('\u{1F525}') || text.contains("widget ID") {
                    let rect = egui::Rect::from_min_size(text_shape.pos, egui::Vec2::ZERO);
                    hits.push((format!("id-clash text: {text}"), rect));
                }
            }
            egui::Shape::Rect(rect_shape) => {
                if rect_shape.stroke.color == egui::Color32::from_rgb(255, 0, 0) {
                    hits.push((
                        format!("red rect_stroke width={}", rect_shape.stroke.width),
                        rect_shape.rect,
                    ));
                }
            }
            _ => {}
        }
    }

    fn assert_no_id_clash_text(harness: &Harness<'static, WavesPreviewer>, when: &str) {
        let mut hits = Vec::new();
        for clipped in &harness.output().shapes {
            collect_id_clash_shapes(&clipped.shape, &mut hits);
        }
        assert!(
            hits.is_empty(),
            "id clash overlay detected {when}: {hits:?}"
        );
    }

    #[test]
    fn list_view_no_id_clash_during_scroll() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);

        let file_count = harness.state().files.len();
        assert!(
            file_count >= 20,
            "need at least 20 files to exercise list scrolling, got {file_count}"
        );

        harness.run_steps(3);
        assert_no_id_clash_text(&harness, "baseline");

        let hover_pos = egui::pos2(640.0, 200.0);
        harness.hover_at(hover_pos);
        harness.run_steps(1);
        assert_no_id_clash_text(&harness, "after hover");

        for i in 0..40 {
            harness.event(egui::Event::MouseWheel {
                unit: MouseWheelUnit::Line,
                delta: egui::vec2(0.0, -3.0),
                phase: egui::TouchPhase::Move,
                modifiers: Modifiers::default(),
            });
            harness.run_steps(1);
            assert_no_id_clash_text(&harness, &format!("during scroll down step {i}"));
        }

        for i in 0..40 {
            harness.event(egui::Event::MouseWheel {
                unit: MouseWheelUnit::Line,
                delta: egui::vec2(0.0, 3.0),
                phase: egui::TouchPhase::Move,
                modifiers: Modifiers::default(),
            });
            harness.run_steps(1);
            assert_no_id_clash_text(&harness, &format!("during scroll up step {i}"));
        }

        // `warn_if_rect_changes_id` only fires when a widget rect is bit-identical
        // between two consecutive passes but its `Id` changed. Table row rects shift
        // by exactly `row_h` per fully-scrolled row, so step the scroll offset by
        // exactly one row height (in points) to try to land on that exact alignment.
        let row_h = harness.state().wave_row_h;
        for i in 0..30 {
            harness.event(egui::Event::MouseWheel {
                unit: MouseWheelUnit::Point,
                delta: egui::vec2(0.0, -row_h),
                phase: egui::TouchPhase::Move,
                modifiers: Modifiers::default(),
            });
            harness.run_steps(1);
            assert_no_id_clash_text(
                &harness,
                &format!("during single-row point scroll down step {i}"),
            );
        }
        for i in 0..30 {
            harness.event(egui::Event::MouseWheel {
                unit: MouseWheelUnit::Point,
                delta: egui::vec2(0.0, row_h),
                phase: egui::TouchPhase::Move,
                modifiers: Modifiers::default(),
            });
            harness.run_steps(1);
            assert_no_id_clash_text(
                &harness,
                &format!("during single-row point scroll up step {i}"),
            );
        }
    }

    /// Sanity check for [`collect_id_clash_shapes`]: a deliberate same-frame `Id` clash
    /// between two non-overlapping interactive widgets must be detected.
    #[test]
    fn id_clash_detection_sanity_check() {
        let mut harness = egui_kittest::Harness::new_ui(|ui| {
            let id = egui::Id::new("kittest_sanity_clash");
            ui.interact(
                egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(10.0, 10.0)),
                id,
                egui::Sense::click(),
            );
            ui.interact(
                egui::Rect::from_min_size(egui::pos2(100.0, 100.0), egui::vec2(10.0, 10.0)),
                id,
                egui::Sense::click(),
            );
        });
        harness.run();
        let mut hits = Vec::new();
        for clipped in &harness.output().shapes {
            collect_id_clash_shapes(&clipped.shape, &mut hits);
        }
        assert!(
            !hits.is_empty(),
            "expected id clash overlay to be detected: {hits:?}"
        );
    }

    #[test]
    fn list_view_no_id_clash_during_keyboard_scroll_jump() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);

        let file_count = harness.state().files.len();
        assert!(
            file_count >= 20,
            "need at least 20 files to exercise list scrolling, got {file_count}"
        );

        // Give the list focus via an initial selection.
        harness.state_mut().selected = Some(0);
        harness.run_steps(2);
        assert_no_id_clash_text(&harness, "after initial selection");

        for i in 0..10 {
            // Jump to the last row (forces TableBuilder::scroll_to_row to a far offset).
            harness.key_press(Key::End);
            harness.run_steps(2);
            assert_no_id_clash_text(&harness, &format!("after End jump {i}"));

            // Jump back to the first row.
            harness.key_press(Key::Home);
            harness.run_steps(2);
            assert_no_id_clash_text(&harness, &format!("after Home jump {i}"));
        }

        for i in 0..10 {
            harness.key_press(Key::PageDown);
            harness.run_steps(2);
            assert_no_id_clash_text(&harness, &format!("after PageDown {i}"));
        }
        for i in 0..10 {
            harness.key_press(Key::PageUp);
            harness.run_steps(2);
            assert_no_id_clash_text(&harness, &format!("after PageUp {i}"));
        }
    }

    #[test]
    fn inspector_tool_combo_reachable() {
        let mut harness = harness_with_wavs(true);
        wait_for_scan(&mut harness);
        wait_for_tab(&mut harness);

        // The tool picker is an icon toolbar; the Loop Edit icon is its anchor.
        let tool_nodes: Vec<_> = harness.query_all_by_label("🔁").collect();
        assert!(!tool_nodes.is_empty(), "Inspector tool toolbar not found");

        assert!(harness.state_mut().test_set_active_tool(ToolKind::Reverse));
        harness.run_steps(1);
        assert_eq!(harness.state().test_active_tool(), Some(ToolKind::Reverse));
    }

    #[test]
    fn topbar_activity_does_not_move_search_or_meter_controls() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        harness.run_steps(3);

        let search_before = harness
            .state()
            .test_topbar_search_rect()
            .expect("search rect before");
        let volume_before = harness
            .state()
            .test_topbar_volume_rect()
            .expect("volume rect before");
        let meter_before = harness
            .state()
            .test_topbar_output_meter_rect()
            .expect("meter rect before");

        assert!(harness
            .state_mut()
            .test_set_mock_active_tab_processing("Rendering preview..."));
        harness.run_steps(3);

        assert_rect_nearly_same(
            search_before,
            harness
                .state()
                .test_topbar_search_rect()
                .expect("search rect after"),
            "topbar search",
        );
        assert_rect_nearly_same(
            volume_before,
            harness
                .state()
                .test_topbar_volume_rect()
                .expect("volume rect after"),
            "topbar volume",
        );
        assert_rect_nearly_same(
            meter_before,
            harness
                .state()
                .test_topbar_output_meter_rect()
                .expect("meter rect after"),
            "topbar meter",
        );
        harness.state_mut().test_clear_mock_processing();
    }

    #[test]
    fn inspector_controls_stay_fixed_when_activity_changes() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness
            .state_mut()
            .test_set_active_tool(ToolKind::Normalize));
        harness.run_steps(3);
        #[cfg(feature = "kittest_render")]
        render_ui_stability_png(&mut harness, "inspector_controls_idle.png");
        let inspector = first_label_rect(&harness, "Inspector");
        let undo = first_label_rect(&harness, "Undo");
        let range_before = first_label_rect(&harness, "Range: -");
        let tool_before = first_label_rect(&harness, "🔁");
        let preview_before = first_label_rect(&harness, "Preview");
        let apply_before = first_label_rect(&harness, "Apply");
        assert!(
            undo.top() - inspector.bottom() < 40.0,
            "idle inspector should not reserve an empty activity slot: inspector={inspector:?} undo={undo:?}"
        );

        assert!(harness
            .state_mut()
            .test_set_mock_active_tab_processing("Rendering preview..."));
        harness.run_steps(3);
        #[cfg(feature = "kittest_render")]
        render_ui_stability_png(&mut harness, "inspector_controls_processing.png");

        assert_rect_nearly_same(
            range_before,
            first_label_rect(&harness, "Range: -"),
            "range row during processing",
        );
        assert_rect_nearly_same(
            tool_before,
            first_label_rect(&harness, "🔁"),
            "tool picker during processing",
        );
        assert_rect_nearly_same(
            preview_before,
            first_label_rect(&harness, "Preview"),
            "Preview button during processing",
        );
        assert_rect_nearly_same(
            apply_before,
            first_label_rect(&harness, "Apply"),
            "Apply button during processing",
        );
        harness.state_mut().test_clear_mock_processing();
        harness.run_steps(3);
        assert_rect_nearly_same(
            range_before,
            first_label_rect(&harness, "Range: -"),
            "range row after processing",
        );
        assert_rect_nearly_same(
            tool_before,
            first_label_rect(&harness, "🔁"),
            "tool picker after processing",
        );
        assert_rect_nearly_same(
            preview_before,
            first_label_rect(&harness, "Preview"),
            "Preview button after processing",
        );
        assert_rect_nearly_same(
            apply_before,
            first_label_rect(&harness, "Apply"),
            "Apply button after processing",
        );
    }

    #[test]
    fn editor_layout_has_valid_canvas_and_inspector_at_common_sizes() {
        for size in [
            egui::vec2(760.0, 540.0),
            egui::vec2(1160.0, 720.0),
            egui::vec2(1600.0, 900.0),
        ] {
            let mut harness = harness_with_editor_fixture();
            harness.set_size(size);
            wait_for_scan(&mut harness);
            ensure_editor_ready(&mut harness);
            harness.run_steps(4);

            let inspector = first_label_rect(&harness, "Inspector");
            let nav = harness
                .state()
                .test_tab_amplitude_nav_rect()
                .expect("amplitude nav rect");
            assert!(
                inspector.height() >= 18.0 && inspector.top() < size.y - 32.0,
                "inspector should be visible at size {size:?}: {inspector:?}"
            );
            assert!(
                nav.width() >= 12.0 && nav.height() >= 120.0,
                "canvas amplitude nav should be visible at size {size:?}: {nav:?}"
            );
            assert!(
                harness.state().test_topbar_volume_rect().is_some()
                    && harness.state().test_topbar_output_meter_rect().is_some()
                    && harness.state().test_topbar_search_rect().is_some(),
                "topbar control rects should be recorded at size {size:?}"
            );
        }
    }

    #[test]
    fn loop_detect_progress_slot_does_not_move_loop_inspector() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness.state_mut().test_set_active_tool(ToolKind::LoopEdit));
        harness.run_steps(3);
        let before = first_label_rect(&harness, "Seam Check");

        assert!(harness
            .state_mut()
            .test_set_mock_loop_detect_running(0.42, "Scoring loop candidates... 42%"));
        harness.run_steps(3);

        assert_rect_nearly_same(
            before,
            first_label_rect(&harness, "Seam Check"),
            "loop inspector row",
        );
        assert!(harness.state_mut().test_clear_mock_loop_detect());
    }

    #[test]
    fn auto_trim_progress_slot_keeps_trim_section_stable() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness.state_mut().test_set_active_tool(ToolKind::Trim));
        harness.run_steps(3);
        let before = first_label_rect(&harness, "Auto Trim");

        assert!(harness
            .state_mut()
            .test_set_mock_auto_trim_running(0.55, "Auto Trim detecting sections... 55%"));
        harness.run_steps(3);

        assert_rect_nearly_same(
            before,
            first_label_rect(&harness, "Auto Trim"),
            "Auto Trim header",
        );
        assert!(harness.state_mut().test_clear_mock_auto_trim());
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
        assert!(
            harness.state().test_active_tab_loading_waveform_ready(),
            "the loading placeholder overview should be present while decoding"
        );

        // Wait for the decode to surface a visual length. The final decode
        // result clears the loading overview in the same frame it applies, so
        // "visual > 0 while the overview is still up" is a race the test must
        // not depend on; instead assert the invariant "loading implies the
        // overview is visible" on every observed frame.
        let start = Instant::now();
        loop {
            harness.run_steps(1);
            let loading = harness.state().test_tab_loading();
            if loading {
                assert!(
                    harness.state().test_active_tab_loading_waveform_ready(),
                    "loading overview must stay visible while the decode streams"
                );
            }
            if harness.state().test_active_tab_samples_len_visual() > 0 && !loading {
                break;
            }
            if start.elapsed() > Duration::from_secs(10) {
                panic!(
                    "visual length never updated after decode started: visual={} tab_loading={}",
                    harness.state().test_active_tab_samples_len_visual(),
                    harness.state().test_tab_loading(),
                );
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn disconnected_editor_decode_worker_cannot_leave_tab_loading_forever() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);

        assert!(harness
            .state_mut()
            .test_set_mock_editor_decode_progress(0.60));
        assert!(harness.state_mut().test_disconnect_mock_editor_decode());
        harness.run_steps(2);

        assert!(
            !harness.state().test_tab_loading(),
            "a disconnected waveform worker must clear the loading state"
        );
        assert!(
            harness.state().test_editor_decode_progress().is_none(),
            "the disconnected worker state must be retired"
        );
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
            (neowaves::ViewMode::World, "World (F0/Env)", "World"),
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
            "World",
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
        let _ = harness.get_by_label("Return List playback to start when stopped");
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
    fn list_stop_return_to_start_pref_roundtrip_persists() {
        let mut harness = harness_empty();
        let prefs = make_temp_dir("prefs_list_stop_return").join("prefs.txt");
        assert!(!harness.state().test_list_stop_returns_to_start());

        harness
            .state_mut()
            .test_set_list_stop_returns_to_start(true);
        harness.state().test_save_prefs_to_path(&prefs);
        harness
            .state_mut()
            .test_set_list_stop_returns_to_start(false);
        harness.state_mut().test_load_prefs_from_path(&prefs);
        assert!(harness.state().test_list_stop_returns_to_start());

        harness
            .state_mut()
            .test_set_list_stop_returns_to_start(false);
        harness.state().test_save_prefs_to_path(&prefs);
        harness
            .state_mut()
            .test_set_list_stop_returns_to_start(true);
        harness.state_mut().test_load_prefs_from_path(&prefs);
        assert!(!harness.state().test_list_stop_returns_to_start());
    }

    #[test]
    fn recent_sessions_pref_roundtrip_filters_legacy_sessions() {
        let dir = make_temp_dir("recent_prefs");
        let first = dir.join("first.nwsess");
        let second = dir.join("second.nwsess");
        let third = dir.join("third.nwsess");
        let fourth = dir.join("fourth.nwsess");
        let bad = dir.join("bad.nwproj");
        for path in [&first, &second, &third, &fourth, &bad] {
            std::fs::write(path, "placeholder").expect("write placeholder session");
        }
        let prefs = dir.join("prefs.txt");
        let mut harness = harness_empty();
        harness.state_mut().test_set_recent_session_paths(vec![
            first.clone(),
            second.clone(),
            bad,
            third.clone(),
            fourth.clone(),
        ]);
        harness.state().test_save_prefs_to_path(&prefs);
        harness
            .state_mut()
            .test_set_recent_session_paths(Vec::new());
        harness.state_mut().test_load_prefs_from_path(&prefs);

        let recents = harness.state().test_recent_session_paths();
        assert_eq!(recents.len(), 4);
        assert_eq!(recents[0], std::fs::canonicalize(&first).unwrap());
        assert_eq!(recents[1], std::fs::canonicalize(&second).unwrap());
        assert_eq!(recents[2], std::fs::canonicalize(&third).unwrap());
        assert_eq!(recents[3], std::fs::canonicalize(&fourth).unwrap());
    }

    #[test]
    fn recent_sessions_file_menu_click_opens_session() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        let dir = make_temp_dir("recent_menu_open");
        let sess = dir.join("recent_menu_open.nwsess");
        assert!(harness.state_mut().test_save_session_to(&sess));
        assert!(harness.state_mut().test_close_session_with_autosave());
        harness.run_steps(2);

        top_menu_button(&harness, "File").click();
        harness.run_steps(1);
        harness.get_by_label("Recent Sessions ⏵").click();
        harness.run_steps(1);
        harness.get_by_label("1  recent_menu_open.nwsess").click();
        wait_for_project_path(&mut harness, &sess);
        wait_for_tab_ready(&mut harness);

        assert_eq!(
            harness.state().test_project_path(),
            Some(std::fs::canonicalize(&sess).unwrap_or(sess.clone()))
        );
    }

    #[test]
    fn recent_session_close_autosaves_and_reopens_editor_state() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        let dir = make_temp_dir("recent_close_autosave");
        let sess = dir.join("recent_close_autosave.nwsess");
        assert!(harness.state_mut().test_save_session_to(&sess));

        assert!(harness.state_mut().test_set_active_tool(ToolKind::LoopEdit));
        assert!(harness.state_mut().test_set_selection_frac(0.18, 0.33));
        assert!(harness.state_mut().test_set_loop_region_frac(0.42, 0.64));
        let base_spp = harness
            .state()
            .test_tab_samples_per_px()
            .unwrap_or(128.0)
            .max(4.0);
        assert!(harness
            .state_mut()
            .test_set_tab_samples_per_px(base_spp * 0.35));
        let view_offset = harness.state().tabs[harness.state().active_tab.unwrap()].samples_len / 5;
        assert!(harness.state_mut().test_set_tab_view_offset(view_offset));
        let saved_selection = harness.state().test_tab_selection();
        let saved_loop = harness.state().test_loop_region();
        let saved_view = harness.state().test_tab_view_offset();
        let saved_path = harness.state().test_active_tab_path();

        assert!(harness.state_mut().test_close_session_with_autosave());
        harness.run_steps(2);
        assert_eq!(harness.state().test_project_path(), None);
        assert!(harness.state().active_tab.is_none());
        let recent = harness
            .state()
            .test_recent_session_paths()
            .into_iter()
            .next()
            .expect("recent session");
        assert_eq!(recent, std::fs::canonicalize(&sess).unwrap_or(sess.clone()));

        assert!(harness.state_mut().test_open_session_from(&recent));
        wait_for_tab_ready(&mut harness);

        assert_eq!(harness.state().test_active_tool(), Some(ToolKind::LoopEdit));
        assert_eq!(harness.state().test_tab_selection(), saved_selection);
        assert_eq!(harness.state().test_loop_region(), saved_loop);
        assert_eq!(harness.state().test_tab_view_offset(), saved_view);
        let reopened_path = harness
            .state()
            .test_active_tab_path()
            .and_then(|p| std::fs::canonicalize(&p).ok().or(Some(p)));
        let saved_path = saved_path.and_then(|p| std::fs::canonicalize(&p).ok().or(Some(p)));
        assert_eq!(reopened_path, saved_path);
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
    fn world_view_runs_analysis_and_caches_features() {
        let _world_guard = world_analysis_test_guard();
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        open_first_tab(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness
            .state_mut()
            .test_set_view_mode(neowaves::ViewMode::World));
        let (frames, bins, voiced_ratio) = wait_for_world_features(&mut harness);
        assert!(frames > 0, "WORLD analysis should produce frames");
        assert!(bins > 0, "WORLD analysis should produce envelope bins");
        assert!(
            (0.0..=1.0).contains(&voiced_ratio),
            "voiced ratio must be a fraction, got {voiced_ratio}"
        );
    }

    #[test]
    fn world_f0_edit_resynthesizes_audio_with_undo() {
        let _world_guard = world_analysis_test_guard();
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        open_first_tab(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness
            .state_mut()
            .test_set_view_mode(neowaves::ViewMode::World));
        wait_for_world_features(&mut harness);
        let tab_idx = harness.state().active_tab.unwrap();
        let fingerprint = |state: &WavesPreviewer| -> f64 {
            state.tabs[tab_idx].ch_samples[0]
                .iter()
                .take(48_000)
                .map(|v| (*v as f64).abs())
                .sum()
        };
        let before_len = harness.state().tabs[tab_idx].samples_len;
        let before_fp = fingerprint(harness.state());
        let undo_before = harness.state().tabs[tab_idx].undo_stack.len();
        assert!(
            harness.state_mut().test_world_shift_and_resynth(12.0),
            "resynthesis job should spawn"
        );
        let start = Instant::now();
        loop {
            harness.run_steps(1);
            if !harness.state().test_editor_apply_busy() {
                break;
            }
            if start.elapsed() > WORLD_ANALYSIS_TIMEOUT {
                panic!("WORLD resynthesis timeout");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        harness.run_steps(2);
        let tab = &harness.state().tabs[tab_idx];
        assert_eq!(tab.samples_len, before_len, "length must be preserved");
        assert!(
            tab.ch_samples.iter().all(|ch| ch.len() == before_len),
            "every channel must carry the resynthesized audio"
        );
        let after_fp = fingerprint(harness.state());
        assert!(
            (after_fp - before_fp).abs() > before_fp * 0.01,
            "audio should change after resynthesis (before={before_fp}, after={after_fp})"
        );
        assert_eq!(
            harness.state().tabs[tab_idx].undo_stack.len(),
            undo_before + 1,
            "resynthesis must push an undo state"
        );
        assert!(harness.state().tabs[tab_idx].dirty, "tab should be dirty");

        // Ctrl+Z must restore the pre-resynthesis audio, keep the worker
        // Arc mirror in sync, and drop the stale WORLD analysis so the
        // view re-analyzes what is audible again.
        harness.key_press_modifiers(Modifiers::COMMAND, Key::Z);
        harness.run_steps(3);
        let restored_fp = fingerprint(harness.state());
        assert!(
            (restored_fp - before_fp).abs() < before_fp * 0.001,
            "undo should restore the original audio (before={before_fp}, restored={restored_fp})"
        );
        {
            let tab = &harness.state().tabs[tab_idx];
            let arc_fp: f64 = tab.ch_samples_arc[0]
                .iter()
                .take(48_000)
                .map(|v| (*v as f64).abs())
                .sum();
            assert!(
                (arc_fp - restored_fp).abs() < restored_fp.abs() * 0.001 + 1e-9,
                "ch_samples_arc must mirror the restored buffers"
            );
        }
        assert!(
            harness.state().test_world_features_ready().is_none(),
            "undo must invalidate the stale WORLD analysis cache"
        );
    }

    #[test]
    fn world_f0_zoom_and_edit_toggles_respond() {
        let _world_guard = world_analysis_test_guard();
        let mut harness = harness_with_wavs(false);
        harness.set_size(egui::vec2(1600.0, 1200.0));
        wait_for_scan(&mut harness);
        open_first_tab(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness
            .state_mut()
            .test_set_view_mode(neowaves::ViewMode::World));
        wait_for_world_features(&mut harness);
        let tab_idx = harness.state().active_tab.unwrap();
        assert!(!harness.state().tabs[tab_idx].world_f0_focus);
        harness
            .get_by_label("F0 zoom (50 Hz - 1.1 kHz axis)")
            .click();
        harness.run_steps(2);
        assert!(
            harness.state().tabs[tab_idx].world_f0_focus,
            "F0 zoom checkbox should toggle the focus flag"
        );
        harness.get_by_label("Edit F0 on canvas").click();
        harness.run_steps(2);
        assert!(
            harness.state().tabs[tab_idx]
                .world_f0_draft
                .as_ref()
                .map(|d| d.edit_enabled)
                .unwrap_or(false),
            "Edit F0 checkbox should enable the draft pencil mode"
        );
    }

    #[test]
    fn editor_mini_meter_populates_state() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        open_first_tab(&mut harness);
        ensure_editor_ready(&mut harness);
        harness.run_steps(8);
        let (spectrum_cols, peak_channels, corr) = harness
            .state()
            .test_mini_meter_state()
            .expect("mini meter state for active tab");
        assert!(
            spectrum_cols > 0,
            "spectrum analyzer columns should be sized after drawing"
        );
        let tab_idx = harness.state().active_tab.unwrap();
        let n_ch = harness.state().tabs[tab_idx].ch_samples.len();
        assert_eq!(
            peak_channels, n_ch,
            "peak meter should track one bar per channel"
        );
        assert!(
            (-1.0..=1.0).contains(&corr),
            "correlation must stay in [-1, 1]"
        );
    }

    #[test]
    #[ignore = "manual perf measurement"]
    fn editor_mini_meter_frame_timing_metrics() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        open_first_tab(&mut harness);
        ensure_editor_ready(&mut harness);
        harness.run_steps(4);
        let steps = 120usize;
        let start = Instant::now();
        for _ in 0..steps {
            harness.run_steps(1);
        }
        let elapsed = start.elapsed();
        let per_ms = elapsed.as_secs_f64() * 1000.0 / steps as f64;
        eprintln!(
            "editor_mini_meter_frame_timing_metrics: steps={} total_ms={:.2} per_frame_ms={:.2}",
            steps,
            elapsed.as_secs_f64() * 1000.0,
            per_ms
        );
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
        wait_for_scan(&mut harness);
        assert!(harness.state().root.is_none());
        assert_eq!(harness.state().items.len(), files.len());
        let selected = harness
            .state()
            .test_selected_path()
            .cloned()
            .expect("selected file after explicit load");
        assert_eq!(selected, files[1]);
    }

    #[test]
    fn folder_load_reports_activity_before_completion() {
        let mut harness = harness_empty();
        let dir = wav_dir();
        harness.state_mut().test_start_folder_load(dir);
        assert!(harness.state().scan_in_progress);
        let activity = harness
            .state()
            .test_topbar_scan_activity_text()
            .unwrap_or_default();
        assert!(
            activity.contains("Scanning folder"),
            "folder load should expose folder activity: {activity}"
        );
        wait_for_scan(&mut harness);
    }

    #[test]
    fn explicit_file_load_reports_activity_and_selects_target() {
        let mut harness = harness_empty();
        let files = sample_wav_files(2);
        harness
            .state_mut()
            .test_start_explicit_file_load(&files, true, false, true);
        assert!(harness.state().scan_in_progress);
        let activity = harness
            .state()
            .test_topbar_scan_activity_text()
            .unwrap_or_default();
        assert!(
            activity.contains("Loading files"),
            "explicit load should expose file activity: {activity}"
        );
        wait_for_scan(&mut harness);
        let selected = harness
            .state()
            .test_selected_path()
            .cloned()
            .expect("selected file after explicit load");
        assert_eq!(selected, files[1]);
    }

    #[test]
    fn drag_drop_folder_adds_files() {
        let mut harness = harness_empty();
        let dir = wav_dir();
        let added = harness.state_mut().test_simulate_drop_paths(&[dir]);
        wait_for_scan(&mut harness);
        assert!(added > 0);
        assert!(!harness.state().items.is_empty());
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
        assert!(harness.state().test_tab_dirty());
        wait_for_waveform_pyramid(&mut harness);
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
    fn marker_inspector_is_apply_only_without_preview() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness.state_mut().test_set_active_tool(ToolKind::Markers));
        harness.run_steps(3);

        assert!(
            harness.query_all_by_label("Preview").next().is_none(),
            "Markers are metadata edits and must not expose Preview"
        );
        assert!(
            harness.query_all_by_label("Apply").next().is_some(),
            "Markers must retain Apply"
        );

        #[cfg(feature = "kittest_render")]
        render_ui_stability_png(&mut harness, "markers_apply_only.png");
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
            neowaves::ViewMode::World,
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

        assert!(harness.state().test_loop_region().is_some());
        assert!(!harness
            .query_all_by_label("Seam Check")
            .collect::<Vec<_>>()
            .is_empty());
        assert!(!harness
            .query_all_by_label("Auto Detect")
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
        assert_eq!(
            selection.0, anchor,
            "shift+click should keep the existing anchor"
        );
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
    fn editor_zoom_and_page_keys_navigate_view() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        let spp_start = harness
            .state()
            .test_tab_samples_per_px()
            .expect("spp start");
        harness.key_press(Key::Plus);
        harness.run_steps(3);
        let spp_in = harness.state().test_tab_samples_per_px().expect("spp in");
        assert!(
            spp_in < spp_start,
            "+ should zoom in: start={spp_start} in={spp_in}"
        );
        // `=` shares the physical key with `+` and must act as zoom-in too.
        harness.key_press(Key::Equals);
        harness.run_steps(3);
        let spp_eq = harness
            .state()
            .test_tab_samples_per_px()
            .expect("spp equals");
        assert!(
            spp_eq < spp_in,
            "= should also zoom in: in={spp_in} eq={spp_eq}"
        );
        harness.key_press(Key::Minus);
        harness.run_steps(3);
        let spp_out = harness.state().test_tab_samples_per_px().expect("spp out");
        assert!(
            spp_out > spp_eq,
            "- should zoom out: eq={spp_eq} out={spp_out}"
        );
        // Page keys shift the view one visible width at a time.
        for _ in 0..8 {
            harness.key_press(Key::Plus);
            harness.run_steps(1);
        }
        harness.run_steps(2);
        assert!(harness.state_mut().test_set_tab_view_offset(0));
        harness.run_steps(1);
        harness.key_press(Key::CloseBracket);
        harness.run_steps(3);
        let after_fwd = harness
            .state()
            .test_tab_view_offset()
            .expect("view offset forward");
        assert!(after_fwd > 0, "] should page the view forward");
        harness.key_press(Key::OpenBracket);
        harness.run_steps(3);
        let after_back = harness
            .state()
            .test_tab_view_offset()
            .expect("view offset back");
        assert!(
            after_back < after_fwd,
            "[ should page the view back: fwd={after_fwd} back={after_back}"
        );
    }

    #[test]
    fn editor_wheel_scroll_mode_pans_instead_of_zooming() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        for _ in 0..8 {
            editor_zoom_in_once(&mut harness);
        }
        let tab_idx = harness.state().active_tab.expect("active tab");
        let base_view = harness.state().tabs[tab_idx].samples_len / 2;
        assert!(harness.state_mut().test_set_tab_view_offset(base_view));
        harness.state_mut().test_set_editor_pref_wheel_scrolls(true);
        harness.run_steps(1);
        let spp_before = harness
            .state()
            .test_tab_samples_per_px()
            .expect("samples_per_px before");
        editor_plain_vertical_wheel_once(&mut harness);
        let spp_after = harness
            .state()
            .test_tab_samples_per_px()
            .expect("samples_per_px after");
        assert!(
            (spp_after - spp_before).abs() < 1e-6,
            "plain wheel must not zoom in scroll mode: before={spp_before} after={spp_after}"
        );
        let view_after = harness
            .state()
            .test_tab_view_offset()
            .expect("view offset after");
        assert_ne!(
            view_after, base_view,
            "plain wheel should pan the view in scroll mode"
        );
        // Ctrl/Cmd+wheel still zooms in scroll mode.
        editor_zoom_in_once(&mut harness);
        let spp_zoomed = harness
            .state()
            .test_tab_samples_per_px()
            .expect("samples_per_px zoomed");
        assert!(
            spp_zoomed < spp_after,
            "ctrl+wheel should keep zooming in scroll mode: after={spp_after} zoomed={spp_zoomed}"
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
    fn list_note_editor_notes_and_position_mode_roundtrip_in_session() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        let dir = make_temp_dir("editor_notes_session");
        let sess = dir.join("editor_notes.nwsess");
        let tab_idx = harness.state().active_tab.expect("active tab");
        let path = harness.state().tabs[tab_idx].path.clone();
        let notes = vec![
            EditorNote {
                id: 10,
                comment: "Session cursor".to_string(),
                start_sample: 12_000,
                end_sample: None,
                freq_range_hz: None,
                view: None,
            },
            EditorNote {
                id: 11,
                comment: "Session spectrum".to_string(),
                start_sample: 24_000,
                end_sample: Some(48_000),
                freq_range_hz: Some((330.0, 3_300.0)),
                view: Some("Spectrogram".to_string()),
            },
        ];
        {
            let state = harness.state_mut();
            let item = state
                .items
                .iter_mut()
                .find(|item| item.path == path)
                .expect("session item");
            item.note = "Persisted list note".to_string();
            item.editor_notes = notes.clone();
            state.tabs[tab_idx].editor_notes = notes.clone();
            state.tabs[tab_idx].editor_note_position_mode = EditorNotePositionMode::Beats;
            let note_pos = state
                .list_column_layout
                .iter()
                .position(|key| *key == ColumnKey::Builtin(ColumnId::Note))
                .expect("Note column");
            let note_key = state.list_column_layout.remove(note_pos);
            state.list_column_layout.insert(0, note_key);
        }
        assert!(harness.state_mut().test_save_session_to(&sess));
        {
            let state = harness.state_mut();
            state.items.iter_mut().for_each(|item| {
                item.note.clear();
                item.editor_notes.clear();
            });
            state.tabs[tab_idx].editor_notes.clear();
            state.tabs[tab_idx].editor_note_position_mode = EditorNotePositionMode::Time;
            state.list_column_layout = ColumnId::ALL
                .iter()
                .copied()
                .map(ColumnKey::Builtin)
                .collect();
        }
        assert!(harness.state_mut().test_open_session_from(&sess));
        harness.run_steps(3);
        let restored_item = harness
            .state()
            .items
            .iter()
            .find(|item| item.path == path)
            .expect("restored item");
        assert_eq!(restored_item.note, "Persisted list note");
        assert_eq!(restored_item.editor_notes.len(), 2);
        assert_eq!(
            restored_item.editor_notes[1].freq_range_hz,
            Some((330.0, 3_300.0))
        );
        let restored_tab = harness.state().active_tab.expect("restored active tab");
        assert_eq!(
            harness.state().tabs[restored_tab].editor_note_position_mode,
            EditorNotePositionMode::Beats
        );
        assert_eq!(
            harness.state().list_column_layout.first(),
            Some(&ColumnKey::Builtin(ColumnId::Note))
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

    /// In-place destructive applies rebuild the waveform pyramid on a
    /// background worker; wait for the refreshed cache to land.
    fn wait_for_waveform_pyramid(harness: &mut Harness<'static, WavesPreviewer>) {
        let start = Instant::now();
        loop {
            harness.run_steps(1);
            if harness.state().test_active_tab_waveform_pyramid_ready() {
                break;
            }
            if start.elapsed() > Duration::from_secs(20) {
                panic!("waveform pyramid rebuild timeout");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn editor_apply_gain_rebuilds_waveform_cache() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness.state().test_active_tab_waveform_pyramid_ready());
        assert!(harness.state_mut().test_apply_gain(0.2, 0.6, -6.0));
        wait_for_waveform_pyramid(&mut harness);
    }

    #[test]
    fn editor_apply_reverse_rebuilds_waveform_cache() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness.state().test_active_tab_waveform_pyramid_ready());
        assert!(harness.state_mut().test_apply_reverse(0.1, 0.4));
        wait_for_waveform_pyramid(&mut harness);
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
        wait_for_waveform_pyramid(&mut harness);
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
        wait_for_waveform_pyramid(&mut harness);

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

        harness.get_by_label("Preview").click();
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
    fn editor_normalize_preview_and_apply_buttons_hit_target_peak() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);

        assert!(harness
            .state_mut()
            .test_set_active_tool(ToolKind::Normalize));
        harness.run_steps(2);

        let source_peak = active_tab_peak(harness.state());
        assert!(
            (source_peak - 0.30).abs() < 0.01,
            "fixture peak should make normalization observable: {source_peak}"
        );

        assert!(harness
            .state_mut()
            .test_force_active_tab_buffer_transport(48_000));
        let source_meter_pos = audio_buffer_len(harness.state()) / 2;
        harness
            .state_mut()
            .test_audio_seek_to_sample(source_meter_pos);
        harness.state_mut().test_set_audio_playing_flag(true);
        harness.run_steps(3);
        let source_audio_pos = harness.state().test_audio_play_pos();
        let source_display_pos = harness
            .state()
            .test_audio_play_pos_display()
            .expect("source display playhead");
        assert!(
            source_audio_pos > source_meter_pos / 2 && source_display_pos > source_meter_pos / 2,
            "source playhead should stay near the meter fixture midpoint: audio={source_audio_pos} display={source_display_pos}"
        );
        let tab_idx = harness.state().active_tab.expect("active editor tab");
        let source_meter_peak = harness.state().tabs[tab_idx]
            .mini_meter
            .peak_hold_db
            .iter()
            .copied()
            .fold(f32::NEG_INFINITY, f32::max);
        assert!(
            (source_meter_peak - 20.0 * source_peak.log10()).abs() < 0.5,
            "source Mini Meter should follow the editor samples: {source_meter_peak} dB"
        );
        #[cfg(feature = "kittest_render")]
        render_ui_stability_png(&mut harness, "normalize_meter_source.png");
        harness.state_mut().test_set_audio_playing_flag(false);

        harness.get_by_label("Preview").click();
        wait_for_preview_tool(&mut harness, ToolKind::Normalize, true);

        assert_eq!(
            harness.state().test_preview_audio_tool(),
            Some(ToolKind::Normalize)
        );
        assert_eq!(
            harness.state().test_preview_overlay_tool(),
            Some(ToolKind::Normalize)
        );
        let target_peak = 10.0_f32.powf(-6.0 / 20.0);
        assert!(
            (audio_buffer_peak(harness.state()) - target_peak).abs() < 0.002,
            "Preview audio should peak at -6 dBFS"
        );
        let preview_meter_pos = audio_buffer_len(harness.state()) / 2;
        harness
            .state_mut()
            .test_audio_seek_to_sample(preview_meter_pos);
        harness.state_mut().test_set_audio_playing_flag(true);
        harness.run_steps(3);
        let preview_meter_peak = harness.state().tabs[tab_idx]
            .mini_meter
            .peak_hold_db
            .iter()
            .copied()
            .fold(f32::NEG_INFINITY, f32::max);
        assert!(
            (preview_meter_peak + 6.0).abs() < 0.5,
            "Preview Mini Meter should analyze the normalized audition buffer: {preview_meter_peak} dB"
        );
        assert!(
            preview_meter_peak > source_meter_peak + 3.5,
            "Preview Mini Meter must visibly differ from the source: source={source_meter_peak} preview={preview_meter_peak}"
        );

        #[cfg(feature = "kittest_render")]
        render_ui_stability_png(&mut harness, "normalize_preview.png");
        harness.state_mut().test_set_audio_playing_flag(false);

        harness.get_by_label("Apply").click();
        harness.run_steps(3);

        assert!(harness.state().test_tab_dirty());
        assert_eq!(harness.state().test_preview_audio_tool(), None);
        assert_eq!(harness.state().test_preview_overlay_tool(), None);
        assert!(
            (active_tab_peak(harness.state()) - target_peak).abs() < 0.002,
            "Apply should write -6 dBFS samples into the editor tab"
        );
        assert!(
            (audio_buffer_peak(harness.state()) - target_peak).abs() < 0.002,
            "Apply should update the playback buffer"
        );

        #[cfg(feature = "kittest_render")]
        render_ui_stability_png(&mut harness, "normalize_applied.png");
    }

    #[test]
    fn editor_play_uses_visible_green_preview_audio_after_source_restore() {
        let mut harness = harness_with_editor_fixture();
        harness.set_size(egui::vec2(1600.0, 900.0));
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);

        assert!(harness
            .state_mut()
            .test_set_active_tool(ToolKind::Normalize));
        harness.run_steps(2);
        harness.get_by_label("Preview").click();
        wait_for_preview_tool(&mut harness, ToolKind::Normalize, true);
        assert!(harness.state().test_visible_preview_audio_is_retained());

        // Simulate a later transport/tab activation restoring source audio
        // while the green Preview overlay remains visible.
        assert!(harness
            .state_mut()
            .test_force_active_tab_buffer_transport(48_000));
        harness.state_mut().test_set_audio_playing_flag(false);
        harness.run_steps(2);
        assert_eq!(
            harness.state().test_preview_overlay_tool(),
            Some(ToolKind::Normalize)
        );
        assert!(!harness.state().test_playback_source_is_tool_preview());
        let source_peak = audio_buffer_peak(harness.state());
        assert!(
            (source_peak - 0.30).abs() < 0.01,
            "fixture source should be active before Play: {source_peak}"
        );

        #[cfg(feature = "kittest_render")]
        {
            let out_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("debug")
                .join("screenshot_verify")
                .join("preview_playback");
            std::fs::create_dir_all(&out_dir).expect("create preview playback screenshot dir");
            harness
                .render()
                .expect("render stopped green preview")
                .save(out_dir.join("green_preview_source_stopped.png"))
                .expect("save stopped green preview");
        }

        harness.state_mut().test_request_workspace_play_toggle();
        harness.run_steps(3);

        assert!(harness.state().test_audio_is_playing());
        assert!(harness.state().test_playback_source_is_tool_preview());
        assert_eq!(
            harness.state().test_preview_overlay_tool(),
            Some(ToolKind::Normalize)
        );
        let target_peak = 10.0_f32.powf(-6.0 / 20.0);
        assert!(
            (audio_buffer_peak(harness.state()) - target_peak).abs() < 0.002,
            "Play must reactivate the normalized buffer represented by the green waveform"
        );

        #[cfg(feature = "kittest_render")]
        {
            harness.get_by_label("Playing");
            let out_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("debug")
                .join("screenshot_verify")
                .join("preview_playback");
            harness
                .render()
                .expect("render playing green preview")
                .save(out_dir.join("green_preview_playing.png"))
                .expect("save playing green preview");
        }
    }

    #[test]
    fn editor_long_normalize_default_target_builds_simplified_preview() {
        let mut harness = harness_with_long_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        wait_for_tab_fully_loaded(&mut harness);

        assert!(harness
            .state_mut()
            .test_set_active_tool(ToolKind::Normalize));
        harness.run_steps(2);
        harness.get_by_label("Preview").click();
        for frame in 0..12 {
            harness.run_steps(1);
            if harness.state().test_preview_busy_for_active_tab() {
                assert!(
                    harness.state().test_preview_audio_tool() == Some(ToolKind::Normalize)
                        || harness.state().test_preview_overlay_tool() == Some(ToolKind::Normalize),
                    "Normalize preview disappeared while rebuilding on frame {frame}"
                );
            }
        }
        wait_for_preview_tool(&mut harness, ToolKind::Normalize, true);

        assert_eq!(
            harness.state().test_preview_overlay_tool(),
            Some(ToolKind::Normalize),
            "the default -6 dB target must not be treated as a no-op"
        );
        assert!(harness.state().test_preview_overlay_is_overview_only());
        let target_peak = 10.0_f32.powf(-6.0 / 20.0);
        assert!(
            (audio_buffer_peak(harness.state()) - target_peak).abs() < 0.002,
            "long-clip Preview should provide normalized audition audio"
        );

        assert!(harness.state_mut().test_set_tool_normalize_target_db(-9.0));
        assert!(harness.state_mut().test_refresh_tool_preview_active_tab());
        for frame in 0..12 {
            assert_eq!(
                harness.state().test_preview_overlay_tool(),
                Some(ToolKind::Normalize),
                "the previous Normalize overlay must remain visible during rebuild frame {frame}"
            );
            harness.run_steps(1);
        }
        wait_for_preview_idle(&mut harness);
        let rebuilt_peak = 10.0_f32.powf(-9.0 / 20.0);
        assert!(
            (audio_buffer_peak(harness.state()) - rebuilt_peak).abs() < 0.002,
            "replacement Normalize preview should adopt the new target"
        );

        assert!(harness.state_mut().test_set_tool_normalize_target_db(-6.0));
        assert!(harness.state_mut().test_refresh_tool_preview_active_tab());
        assert_eq!(
            harness.state().test_preview_overlay_tool(),
            Some(ToolKind::Normalize),
            "the replacement overlay must stay visible while restoring the target"
        );
        wait_for_preview_idle(&mut harness);
        #[cfg(feature = "kittest_render")]
        render_ui_stability_png(&mut harness, "normalize_long_preview.png");

        harness.get_by_label("Apply").click();
        harness.run_steps(3);
        assert!(harness.state().test_tab_dirty());
        assert!(
            (active_tab_peak(harness.state()) - target_peak).abs() < 0.002,
            "long-clip Apply should write -6 dBFS samples into the editor tab"
        );
        assert!(
            (audio_buffer_peak(harness.state()) - target_peak).abs() < 0.002,
            "long-clip Apply should update the playback buffer"
        );
    }

    #[test]
    fn editor_long_gain_curve_green_preview_has_matching_audio() {
        let mut harness = harness_with_long_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        wait_for_tab_fully_loaded(&mut harness);

        assert!(harness.state_mut().test_set_active_tool(ToolKind::Gain));
        assert!(harness
            .state_mut()
            .test_set_gain_curve(true, &[(0.0, -12.0), (1.0, 6.0)]));
        assert!(harness.state_mut().test_refresh_tool_preview_active_tab());
        wait_for_preview_tool(&mut harness, ToolKind::Gain, true);
        wait_for_preview_idle(&mut harness);

        assert!(harness.state().test_preview_overlay_is_overview_only());
        assert!(harness.state().test_visible_preview_audio_is_retained());
        assert!(harness.state().test_playback_source_is_tool_preview());
        assert!(
            audio_buffer_peak(harness.state()) > 0.5,
            "long Gain-curve Preview audio should follow the +6 dB end of the visible curve"
        );
    }

    #[test]
    fn editor_long_gain_curve_play_waits_for_matching_preview_audio() {
        let mut harness = harness_with_long_editor_fixture();
        harness.set_size(egui::vec2(1600.0, 900.0));
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        wait_for_tab_fully_loaded(&mut harness);

        assert!(harness.state_mut().test_set_active_tool(ToolKind::Gain));
        assert!(harness
            .state_mut()
            .test_set_gain_curve(true, &[(0.0, -12.0), (1.0, 6.0)]));
        harness.run_steps(2);

        #[cfg(feature = "kittest_render")]
        {
            let out_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("debug")
                .join("screenshot_verify")
                .join("gain_curve_playback");
            std::fs::create_dir_all(&out_dir).expect("create Gain playback screenshot dir");
            harness
                .render()
                .expect("render Gain curve before Preview and Play")
                .save(out_dir.join("gain_curve_before_preview_play.png"))
                .expect("save Gain curve before Preview and Play");
        }

        assert!(harness.state_mut().test_refresh_tool_preview_active_tab());
        assert!(
            harness.state().test_preview_busy_for_active_tab(),
            "long Gain-curve Preview should still be rendering before the immediate Play request"
        );
        harness.state_mut().test_request_workspace_play_toggle();
        assert!(
            !harness.state().test_audio_is_playing(),
            "Play must wait instead of auditioning the unprocessed source while Preview is rendering"
        );

        wait_for_preview_tool(&mut harness, ToolKind::Gain, true);
        wait_for_preview_idle(&mut harness);

        assert!(
            harness.state().test_audio_is_playing(),
            "the requested audition should start automatically when the matching Preview is ready"
        );
        assert!(harness.state().test_playback_source_is_tool_preview());
        assert!(
            audio_buffer_peak(harness.state()) > 0.5,
            "autoplay must use the processed Gain-curve buffer"
        );

        #[cfg(feature = "kittest_render")]
        {
            harness.get_by_label("Playing");
            let out_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("debug")
                .join("screenshot_verify")
                .join("gain_curve_playback");
            harness
                .render()
                .expect("render playing Gain preview")
                .save(out_dir.join("gain_curve_preview_playing.png"))
                .expect("save playing Gain preview");
        }
    }

    #[test]
    fn editor_non_gain_background_previews_wait_for_processed_audio_before_playing() {
        for tool in [ToolKind::PitchShift, ToolKind::TimeStretch, ToolKind::Speed] {
            let mut harness = harness_with_dynamic_editor_fixture();
            harness.set_size(egui::vec2(1600.0, 900.0));
            wait_for_scan(&mut harness);
            ensure_editor_ready(&mut harness);

            assert!(harness.state_mut().test_set_active_tool(tool));
            match tool {
                ToolKind::PitchShift => {
                    assert!(harness
                        .state_mut()
                        .test_set_pitch_curve(true, &[(0.0, -5.0), (0.5, 7.0), (1.0, 3.0)],));
                }
                ToolKind::TimeStretch => {
                    assert!(harness.state_mut().test_set_tool_stretch_rate(1.35));
                }
                ToolKind::Speed => {
                    assert!(harness.state_mut().test_set_tool_speed_rate(0.72));
                }
                _ => unreachable!(),
            }
            harness.run_steps(2);

            #[cfg(feature = "kittest_render")]
            if tool == ToolKind::PitchShift {
                let out_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("debug")
                    .join("screenshot_verify")
                    .join("non_gain_preview_playback");
                std::fs::create_dir_all(&out_dir).expect("create non-Gain playback screenshot dir");
                harness
                    .render()
                    .expect("render Pitch curve before Preview and Play")
                    .save(out_dir.join("pitch_curve_before_preview_play.png"))
                    .expect("save Pitch curve before Preview and Play");
            }

            assert!(harness.state_mut().test_refresh_tool_preview_active_tab());
            assert!(
                harness.state().test_preview_busy_for_active_tab(),
                "{tool:?} should have a background Preview in flight"
            );
            harness.state_mut().test_request_workspace_play_toggle();
            assert!(
                !harness.state().test_audio_is_playing(),
                "{tool:?} must not play the unprocessed source while Preview is rendering"
            );

            wait_for_preview_tool(&mut harness, tool, true);
            wait_for_preview_idle(&mut harness);
            assert!(
                harness.state().test_audio_is_playing(),
                "{tool:?} should start automatically with its completed Preview"
            );
            assert!(harness.state().test_playback_source_is_tool_preview());
            assert_eq!(harness.state().test_preview_audio_tool(), Some(tool));
            assert_eq!(harness.state().test_preview_overlay_tool(), Some(tool));
            assert!(harness.state().test_visible_preview_audio_is_retained());
            assert!(
                audio_buffer_peak(harness.state()) > 0.01,
                "{tool:?} Preview should install non-silent processed audio"
            );

            #[cfg(feature = "kittest_render")]
            if tool == ToolKind::PitchShift {
                harness.get_by_label("Playing");
                let out_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("debug")
                    .join("screenshot_verify")
                    .join("non_gain_preview_playback");
                harness
                    .render()
                    .expect("render playing Pitch Preview")
                    .save(out_dir.join("pitch_curve_preview_playing.png"))
                    .expect("save playing Pitch Preview");
            }
        }
    }

    #[test]
    fn editor_music_analyze_preview_waits_for_processed_audio_before_playing() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness
            .state_mut()
            .test_set_active_tool(ToolKind::MusicAnalyze));
        assert!(harness
            .state_mut()
            .test_set_music_analysis_result_mock(true));
        assert!(harness.state_mut().test_set_mock_music_stems_audio(0.08));
        harness.run_steps(2);

        assert!(harness
            .state_mut()
            .test_apply_music_preview_mix_active_tab());
        assert!(harness.state().test_preview_busy_for_active_tab());
        harness.state_mut().test_request_workspace_play_toggle();
        assert!(
            !harness.state().test_audio_is_playing(),
            "Music Analyze must wait for its processed mix instead of playing the source"
        );

        wait_for_preview_tool(&mut harness, ToolKind::MusicAnalyze, true);
        wait_for_preview_idle(&mut harness);
        assert!(harness.state().test_audio_is_playing());
        assert!(harness.state().test_playback_source_is_tool_preview());
        assert_eq!(
            harness.state().test_preview_audio_tool(),
            Some(ToolKind::MusicAnalyze)
        );
        assert!(harness.state().test_visible_preview_audio_is_retained());
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

    #[cfg(feature = "kittest_render")]
    #[test]
    fn kittest_render_edge_fade_tool_drags_and_applies_both_edges_once() {
        let mut harness = harness_with_dynamic_editor_fixture();
        harness.set_size(egui::vec2(1600.0, 900.0));
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness.state_mut().test_set_active_tool(ToolKind::Fade));
        assert!(harness.state_mut().test_set_tool_fade_ms(100.0, 160.0));
        harness.run_steps(5);

        harness.get_by_label("Edge Fade");
        harness.get_by_label("START  ·  FADE IN");
        harness.get_by_label("END  ·  FADE OUT");
        harness.get_by_label("Apply Edge Fades");

        let out_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("debug")
            .join("screenshot_verify")
            .join("edge_fade_tool");
        std::fs::create_dir_all(&out_dir).expect("create Edge Fade screenshot directory");
        harness
            .render()
            .expect("render Edge Fade draft")
            .save(out_dir.join("01_edge_fade_draft.png"))
            .expect("save Edge Fade draft screenshot");

        let tab_idx = harness.state().active_tab.expect("active tab");
        let tab = &harness.state().tabs[tab_idx];
        let clip_ms = tab.samples_len as f32 / tab.buffer_sample_rate.max(1) as f32 * 1000.0;
        let start_fraction = (tab.tool_state.fade_in_ms / clip_ms).clamp(0.0, 1.0);
        let start = editor_canvas_pos_at_frac(&harness, start_fraction);
        let end = editor_canvas_pos_at_frac(&harness, 0.28);
        editor_pointer_drag(&mut harness, start, end);
        let dragged_ms = harness.state().tabs[tab_idx].tool_state.fade_in_ms;
        assert!(
            (dragged_ms - clip_ms * 0.28).abs() <= clip_ms * 0.03,
            "blue fade-in handle should set the draft length: got {dragged_ms} ms for {clip_ms} ms clip"
        );
        harness
            .render()
            .expect("render dragged Edge Fade")
            .save(out_dir.join("02_edge_fade_dragged.png"))
            .expect("save dragged Edge Fade screenshot");

        let undo_before = harness.state().tabs[tab_idx].undo_stack.len();
        harness.get_by_label("Apply Edge Fades").click();
        harness.run_steps(5);
        wait_for_waveform_pyramid(&mut harness);
        let tab = &harness.state().tabs[tab_idx];
        assert_eq!(
            tab.undo_stack.len(),
            undo_before + 1,
            "front and back fades should be one undo step"
        );
        assert!(tab.ch_samples[0][0].abs() <= f32::EPSILON);
        assert!(tab.ch_samples[0][tab.samples_len - 1].abs() <= f32::EPSILON);
        assert_eq!(tab.tool_state.fade_in_ms, 0.0);
        assert_eq!(tab.tool_state.fade_out_ms, 0.0);
        harness
            .render()
            .expect("render applied Edge Fade")
            .save(out_dir.join("03_edge_fade_applied.png"))
            .expect("save applied Edge Fade screenshot");
    }

    #[test]
    fn editor_dsp_and_repair_tools_build_preview_audio_and_overlay() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);

        for tool in [
            ToolKind::NoiseGate,
            ToolKind::Eq,
            ToolKind::Compressor,
            ToolKind::InsertSilence,
            ToolKind::DeClick,
            ToolKind::DeClip,
            ToolKind::DeHum,
        ] {
            assert!(harness.state_mut().test_set_active_tool(tool));
            assert!(harness.state_mut().test_refresh_tool_preview_active_tab());
            wait_for_preview_tool(&mut harness, tool, true);
            assert_eq!(
                harness.state().test_preview_audio_tool(),
                Some(tool),
                "{tool:?} should provide audition audio"
            );
            assert_eq!(
                harness.state().test_preview_overlay_tool(),
                Some(tool),
                "{tool:?} should provide a waveform preview"
            );
            assert!(harness.state_mut().test_force_preview_restore_active_tab());
            harness.run_steps(2);
        }
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
        harness.run_steps(2);
        harness.get_by_label("Downloading transcript model... 3/7");

        harness
            .state_mut()
            .test_clear_mock_model_download_progress();
        harness
            .state_mut()
            .test_set_mock_music_model_download_progress(5, 9);
        harness.run_steps(2);
        harness.get_by_label("Downloading music model... 5/9");
        harness
            .state_mut()
            .test_clear_mock_model_download_progress();
    }

    #[test]
    fn metadata_hex_row_double_click_seeks_and_selects_pcm_frame() {
        let mut harness = harness_with_dynamic_editor_fixture();
        harness.set_size(egui::vec2(1600.0, 900.0));
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness.state_mut().test_set_metadata_view(true));

        let started = Instant::now();
        while !harness.state().test_metadata_document_ready() {
            harness.run_steps(1);
            assert!(
                started.elapsed() < Duration::from_secs(15),
                "metadata document timeout"
            );
            std::thread::sleep(Duration::from_millis(10));
        }

        assert!(harness
            .state_mut()
            .test_force_active_tab_buffer_transport(48_000));
        assert!(harness.state_mut().test_set_metadata_follow_playback(false));
        harness.state_mut().test_set_audio_playing_flag(true);
        harness.run_steps(6);

        let source_len = harness.state().test_audio_source_len();
        assert!(source_len > 0);
        harness.get_by_label("縦波形シークバー").click();
        harness.run_steps(6);
        let waveform_seeked = harness.state().test_audio_play_pos();
        assert!(
            waveform_seeked.abs_diff(source_len / 2) < source_len / 20,
            "waveform click should seek near the middle: pos={waveform_seeked} len={source_len}"
        );
        let waveform_selected = harness
            .state()
            .test_metadata_hex_selection()
            .expect("waveform seek must select the corresponding PCM frame bytes");
        assert_eq!(waveform_selected.length, 8);
        let waveform_fraction = harness
            .state()
            .test_metadata_hex_seek_fraction()
            .expect("waveform cursor fraction");
        assert!((waveform_fraction - 0.5).abs() < 0.05);

        let row_start = waveform_selected.offset / 16 * 16;
        harness.state_mut().test_audio_seek_to_sample(128);
        harness.run_steps(3);

        let row_label = format!("Hex row {row_start:016X}");
        harness.input_mut().time = Some(10.0);
        harness.get_by_label(&row_label).click();
        harness.run_steps(1);
        harness.input_mut().time = Some(10.1);
        harness.get_by_label(&row_label).click();
        harness.run_steps(4);

        let seeked = harness.state().test_audio_play_pos();
        let selection_after_clicks = harness.state().test_metadata_hex_selection();
        let fraction_after_clicks = harness.state().test_metadata_hex_seek_fraction();
        assert!(
            seeked.abs_diff(source_len / 2) <= 4,
            "double-clicking the PCM row should move the waveform/audio cursor back to that row: pos={seeked} selection={selection_after_clicks:?} fraction={fraction_after_clicks:?}"
        );
        let selected = harness
            .state()
            .test_metadata_hex_selection()
            .expect("double-click must select the exact PCM frame bytes");
        assert_eq!(
            selected.length, 8,
            "stereo 32-bit PCM frame must highlight all eight interleaved bytes"
        );
        assert!(harness.state().test_metadata_hex_seek_fraction().is_some());
        harness.state_mut().test_set_audio_playing_flag(false);
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
    fn kittest_render_metadata_structure_and_hex_screenshots() {
        let mut harness = harness_with_dynamic_editor_fixture();
        harness.set_size(egui::vec2(1600.0, 900.0));
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        harness.run_steps(3);

        let out_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("debug")
            .join("screenshot_verify")
            .join("metadata_inspector");
        std::fs::create_dir_all(&out_dir).expect("create metadata screenshot directory");
        harness
            .render()
            .expect("render waveform baseline")
            .save(out_dir.join("01_wave_baseline.png"))
            .expect("save waveform baseline");

        assert!(harness.state_mut().test_set_metadata_view(false));
        let started = Instant::now();
        while !harness.state().test_metadata_document_ready() {
            harness.run_steps(1);
            assert!(
                started.elapsed() < Duration::from_secs(15),
                "metadata document timeout"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(harness.state().test_metadata_node_count() >= 3);
        harness.run_steps(3);
        harness.get_by_label("Details");
        harness.get_by_label("Properties");
        harness
            .render()
            .expect("render Metadata Structure")
            .save(out_dir.join("02_metadata_structure.png"))
            .expect("save Metadata Structure");

        assert!(
            harness.query_by_label("fmt").is_none(),
            "WAV fmt must not appear in the Structure list"
        );
        harness.get_by_label("data").click();
        harness.run_steps(4);
        harness
            .render()
            .expect("render expanded audio preview")
            .save(out_dir.join("02b_metadata_audio.png"))
            .expect("save expanded audio preview");

        assert!(harness.state_mut().test_set_metadata_view(true));
        harness.run_steps(6);
        harness.get_by_label("Offset");
        harness.get_by_label("再生位置に自動スクロール");
        harness
            .render()
            .expect("render Metadata Hex")
            .save(out_dir.join("03_metadata_hex.png"))
            .expect("save Metadata Hex");

        harness.get_by_label("32 bytes").click();
        harness.run_steps(3);
        harness
            .render()
            .expect("render Metadata Hex at 32 bytes per row")
            .save(out_dir.join("03b_metadata_hex_32.png"))
            .expect("save 32-byte Metadata Hex");
        harness.get_by_label("16 bytes").click();
        harness.run_steps(3);

        harness.get_by_label("再生位置に自動スクロール").click();
        assert!(harness
            .state_mut()
            .test_force_active_tab_buffer_transport(48_000));
        harness.state_mut().test_audio_seek_to_sample(4_096);
        harness.state_mut().test_set_audio_playing_flag(true);
        harness.run_steps(6);
        assert!(
            harness.query_by_label("Exact PCM source mapping").is_none(),
            "Hex should not duplicate live PCM details above the grid"
        );
        assert_eq!(
            harness.state().test_metadata_hex_offset(),
            Some(0x8044),
            "{}",
            harness.state().test_metadata_live_mapping_diagnostic()
        );
        harness.get_by_label("Hex row 0000000000008040");
        harness.get_by_label("縦波形シークバー");
        harness
            .render()
            .expect("render Metadata Hex while playing")
            .save(out_dir.join("04_metadata_hex_playing.png"))
            .expect("save playing Metadata Hex");

        let source_len = harness.state().test_audio_source_len();
        assert!(source_len > 0);
        harness.get_by_label("縦波形シークバー").click();
        harness.run_steps(6);
        let seeked = harness.state().test_audio_play_pos();
        assert!(
            seeked.abs_diff(source_len / 2) < source_len / 20,
            "vertical waveform center click should seek near 50%: pos={seeked}, len={source_len}"
        );
        harness
            .render()
            .expect("render Metadata Hex after vertical waveform seek")
            .save(out_dir.join("05_metadata_hex_waveform_seeked.png"))
            .expect("save seeked Metadata Hex");
        harness.state_mut().test_set_audio_playing_flag(false);
    }

    #[cfg(feature = "kittest_render")]
    #[test]
    fn kittest_render_volume_meter_has_no_red_idle_playing_or_stopped() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        harness.run_steps(3);

        render_ui_stability_png(&mut harness, "volume_meter_idle.png");
        harness.state_mut().test_audio_seek_to_sample(10_000);
        harness.key_press(Key::Space);
        render_ui_stability_png(&mut harness, "volume_meter_playing.png");
        harness.key_press(Key::Space);
        render_ui_stability_png(&mut harness, "volume_meter_stopped.png");
        assert!(
            harness.state().test_meter_db() <= -79.9,
            "stopped meter should settle at -inf-equivalent"
        );
    }

    #[cfg(feature = "kittest_render")]
    #[test]
    fn kittest_render_tool_icon_hover_shows_tool_name_without_reflow() {
        let mut harness = harness_with_editor_fixture();
        harness.set_size(egui::vec2(1600.0, 900.0));
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        harness.run_steps(4);

        let out_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("debug")
            .join("screenshot_verify")
            .join("tool_icon_hover");
        std::fs::create_dir_all(&out_dir).expect("create Tool hover screenshot dir");
        let before_path = out_dir.join("01_before_hover.png");
        let after_path = out_dir.join("02_normalize_hover.png");

        let normalize_rect = harness.get_by_label("⬆").rect();
        let inspector_rect = first_label_rect(&harness, "Inspector");
        harness
            .render()
            .expect("render Tool icons before hover")
            .save(&before_path)
            .expect("save Tool icons before hover");

        harness.hover_at(normalize_rect.center());
        harness.run_steps(2);
        assert!(
            harness.query_all_by_label("Normalize").next().is_some(),
            "hovering the Normalize icon should immediately expose its Tool name"
        );
        assert_rect_nearly_same(
            normalize_rect,
            harness.get_by_label("⬆").rect(),
            "Normalize icon while tooltip is visible",
        );
        assert_rect_nearly_same(
            inspector_rect,
            first_label_rect(&harness, "Inspector"),
            "Inspector while Tool tooltip is visible",
        );
        harness
            .render()
            .expect("render Normalize Tool name on hover")
            .save(&after_path)
            .expect("save Normalize Tool hover");
    }

    #[cfg(feature = "kittest_render")]
    #[test]
    fn kittest_render_editor_ui_stability_common_sizes_and_processing_png() {
        for (name, size) in [
            ("layout_760x540_idle.png", egui::vec2(760.0, 540.0)),
            ("layout_1160x720_idle.png", egui::vec2(1160.0, 720.0)),
            ("layout_1600x900_idle.png", egui::vec2(1600.0, 900.0)),
        ] {
            let mut harness = harness_with_editor_fixture();
            harness.set_size(size);
            wait_for_scan(&mut harness);
            ensure_editor_ready(&mut harness);
            harness.run_steps(4);

            let image = render_ui_stability_png(&mut harness, name);
            assert_eq!(image.width(), size.x as u32, "{name}: width");
            assert_eq!(image.height(), size.y as u32, "{name}: height");
            let inspector = first_label_rect(&harness, "Inspector");
            let nav = harness
                .state()
                .test_tab_amplitude_nav_rect()
                .expect("amplitude nav rect");
            assert!(
                inspector.width() >= 80.0 && inspector.height() >= 18.0,
                "{name}: inspector should be visible: {inspector:?}"
            );
            assert!(
                nav.width() >= 12.0 && nav.height() >= 120.0,
                "{name}: editor canvas/nav should keep a usable size: {nav:?}"
            );
        }

        let mut harness = harness_with_editor_fixture();
        harness.set_size(egui::vec2(1160.0, 900.0));
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        harness.run_steps(4);

        let search_before = harness
            .state()
            .test_topbar_search_rect()
            .expect("search rect before processing");
        let volume_before = harness
            .state()
            .test_topbar_volume_rect()
            .expect("volume rect before processing");
        let range_before = first_label_rect(&harness, "Range: -");
        assert!(harness
            .state_mut()
            .test_set_mock_active_tab_processing("Rendering preview..."));
        harness.run_steps(3);
        render_ui_stability_png(&mut harness, "processing_topbar_inspector.png");
        assert_rect_nearly_same(
            search_before,
            harness
                .state()
                .test_topbar_search_rect()
                .expect("search rect during processing"),
            "processing search",
        );
        assert_rect_nearly_same(
            volume_before,
            harness
                .state()
                .test_topbar_volume_rect()
                .expect("volume rect during processing"),
            "processing volume",
        );
        assert_rect_nearly_same(
            range_before,
            first_label_rect(&harness, "Range: -"),
            "processing inspector range",
        );
        harness.state_mut().test_clear_mock_processing();
        harness.run_steps(3);

        harness.set_size(egui::vec2(1600.0, 900.0));
        harness.run_steps(4);
        assert!(harness.state_mut().test_set_active_tool(ToolKind::LoopEdit));
        harness.run_steps(2);
        let loop_before = first_label_rect(&harness, "Seam Check");
        assert!(harness
            .state_mut()
            .test_set_mock_loop_detect_running(0.42, "Scoring loop candidates... 42%"));
        harness.run_steps(3);
        harness.get_by_label("Scoring loop candidates... 42%");
        render_ui_stability_png(&mut harness, "processing_loop_detect.png");
        assert_rect_nearly_same(
            loop_before,
            first_label_rect(&harness, "Seam Check"),
            "loop detect inspector",
        );
        assert!(harness.state_mut().test_clear_mock_loop_detect());

        assert!(harness.state_mut().test_set_active_tool(ToolKind::Trim));
        harness.run_steps(2);
        let trim_before = first_label_rect(&harness, "Auto Trim");
        assert!(harness
            .state_mut()
            .test_set_mock_auto_trim_running(0.55, "Auto Trim detecting sections... 55%"));
        harness.run_steps(3);
        harness.get_by_label("Auto Trim detecting sections... 55%");
        render_ui_stability_png(&mut harness, "processing_auto_trim.png");
        assert_rect_nearly_same(
            trim_before,
            first_label_rect(&harness, "Auto Trim"),
            "auto trim section",
        );
        assert!(harness.state_mut().test_clear_mock_auto_trim());
    }

    #[cfg(feature = "kittest_render")]
    #[test]
    fn kittest_render_inspector_trim_loading_does_not_overflow_png() {
        let mut harness = harness_with_editor_fixture();
        harness.set_size(egui::vec2(1160.0, 900.0));
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness.state_mut().test_set_active_tool(ToolKind::Trim));
        assert!(harness
            .state_mut()
            .test_set_mock_editor_decode_progress(0.88));
        assert!(harness
            .state_mut()
            .test_set_mock_auto_trim_running(0.55, "Auto Trim detecting sections... 55%"));
        harness.run_steps(4);

        harness.get_by_label("Loading exact audio");
        harness.get_by_label("Auto Trim detecting sections... 55%");
        render_ui_stability_png(&mut harness, "inspector_trim_loading_no_overflow.png");
        assert_inspector_labels_inside(
            &harness,
            &[
                "Loading exact audio",
                "below peak (dB)",
                "gap merge (s)",
                "min active (s)",
                "Cancel",
            ],
        );

        harness.state_mut().test_clear_mock_editor_decode_progress();
        assert!(harness.state_mut().test_clear_mock_auto_trim());
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
                phase: egui::TouchPhase::Move,
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
                phase: egui::TouchPhase::Move,
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

    fn wait_for_auto_trim_done(harness: &mut Harness<'static, WavesPreviewer>) {
        let start = Instant::now();
        loop {
            harness.run_steps(1);
            if !harness.state().test_auto_trim_running() {
                break;
            }
            if start.elapsed() > Duration::from_secs(20) {
                panic!(
                    "auto trim timeout message={:?}",
                    harness.state().test_auto_trim_message()
                );
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn editor_apply_trim_range_clears_stale_extra_selections() {
        // Regression test: editor_apply_trim_range used to reset `selection`
        // but leave `extra_selections` untouched, so a stale multi-selection
        // rectangle would survive a single-range Trim (T) and corrupt the
        // grid/waveform redraw afterwards.
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness.state_mut().test_set_selection_frac(0.2, 0.6));
        assert!(harness
            .state_mut()
            .test_set_extra_selections_frac(&[(0.7, 0.9)]));
        assert_eq!(harness.state().test_tab_extra_selections().len(), 1);

        assert!(harness.state_mut().test_apply_trim_frac(0.2, 0.6));
        harness.run_steps(2);

        assert!(
            harness.state().test_tab_selection().is_none(),
            "selection should clear after single-range trim"
        );
        assert!(
            harness.state().test_tab_extra_selections().is_empty(),
            "extra_selections should also clear after single-range trim (latent bug fix)"
        );
    }

    #[test]
    fn editor_delete_range_and_join_clears_stale_extra_selections() {
        // Same latent bug as above, but for the C (delete-and-join) path.
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        let before_len = harness.state().test_tab_samples_len();
        assert!(harness.state_mut().test_set_selection_frac(0.3, 0.5));
        assert!(harness
            .state_mut()
            .test_set_extra_selections_frac(&[(0.7, 0.9)]));
        assert_eq!(harness.state().test_tab_extra_selections().len(), 1);

        assert!(harness.state_mut().test_apply_delete_range_frac(0.3, 0.5));
        harness.run_steps(2);

        let after_len = harness.state().test_tab_samples_len();
        assert!(after_len < before_len, "delete should shorten the buffer");
        assert!(
            harness.state().test_tab_selection().is_none(),
            "selection should clear after single-range delete"
        );
        assert!(
            harness.state().test_tab_extra_selections().is_empty(),
            "extra_selections should also clear after single-range delete (latent bug fix)"
        );
    }

    #[test]
    fn auto_trim_threshold_config_is_persisted_per_tab() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);

        let default_thresholds = harness
            .state()
            .test_auto_trim_config_thresholds_db()
            .expect("auto trim config available for active tab");
        assert!(harness
            .state_mut()
            .test_set_auto_trim_thresholds_db(20.0, 30.0));
        let updated = harness
            .state()
            .test_auto_trim_config_thresholds_db()
            .expect("auto trim config after update");
        assert_eq!(updated, (20.0, 30.0));
        assert_ne!(
            updated, default_thresholds,
            "changing the per-tab config should not silently fall back to defaults"
        );
    }

    #[test]
    fn auto_trim_multi_range_replaces_selection_with_detected_subranges() {
        let mut harness = harness_with_dynamic_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);

        let ranges = [
            (0.05_f32, 0.30_f32),
            (0.40_f32, 0.65_f32),
            (0.75_f32, 0.95_f32),
        ];
        assert!(harness
            .state_mut()
            .test_set_selection_frac(ranges[0].0, ranges[0].1));
        assert!(harness
            .state_mut()
            .test_set_extra_selections_frac(&ranges[1..]));
        let original = harness.state().test_all_selected_ranges();
        assert_eq!(
            original.len(),
            3,
            "expected 3 disjoint selected ranges going into Auto Trim, got {original:?}"
        );

        assert!(harness.state_mut().test_start_auto_trim());
        wait_for_auto_trim_done(&mut harness);
        harness.run_steps(2);

        let detected = harness.state().test_all_selected_ranges();
        assert_eq!(
            detected.len(),
            original.len(),
            "multi-range Auto Trim should produce one detected sub-range per input range \
             (original={original:?} detected={detected:?})"
        );
        for (orig, det) in original.iter().zip(detected.iter()) {
            assert!(
                det.0 >= orig.0 && det.1 <= orig.1 && det.0 <= det.1,
                "detected sub-range {det:?} should stay within its source range {orig:?}"
            );
        }
        assert!(
            harness.state().test_tab_selection().is_some(),
            "primary selection should hold the first detected sub-range"
        );
        assert_eq!(
            harness.state().test_tab_extra_selections().len(),
            detected.len() - 1,
            "remaining detected sub-ranges should populate extra_selections"
        );
    }

    #[test]
    fn auto_trim_no_selection_detects_multiple_sections() {
        let mut harness = harness_with_auto_trim_sections_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);

        assert!(harness.state().test_all_selected_ranges().is_empty());

        assert!(harness.state_mut().test_start_auto_trim());
        wait_for_auto_trim_done(&mut harness);
        harness.run_steps(2);

        let detected = harness.state().test_all_selected_ranges();
        assert_eq!(
            detected.len(),
            2,
            "whole-file Auto Trim should select both separated sections: {detected:?}"
        );
        assert!(
            detected[0].1 < detected[1].0,
            "detected sections should stay separated: {detected:?}"
        );
        let tab_idx = harness.state().active_tab.expect("active tab");
        assert!(
            harness.state().tabs[tab_idx].trim_range.is_none(),
            "multi-section Auto Trim should leave trim_range empty"
        );
    }

    #[test]
    fn auto_trim_inside_single_selection_only_emits_inside_that_range() {
        let mut harness = harness_with_auto_trim_sections_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);

        assert!(harness.state_mut().test_set_selection_frac(0.30, 0.72));
        let original = harness
            .state()
            .test_tab_selection()
            .expect("source selection");

        assert!(harness.state_mut().test_start_auto_trim());
        wait_for_auto_trim_done(&mut harness);
        harness.run_steps(2);

        let detected = harness.state().test_all_selected_ranges();
        assert_eq!(
            detected.len(),
            1,
            "selected subrange should only include the second voice section: {detected:?}"
        );
        assert!(
            detected[0].0 >= original.0 && detected[0].1 <= original.1,
            "detected range {detected:?} should stay inside source selection {original:?}"
        );
        let tab_idx = harness.state().active_tab.expect("active tab");
        assert_eq!(
            harness.state().tabs[tab_idx].trim_range,
            Some(detected[0]),
            "single-section Auto Trim should mirror the selection into trim_range"
        );
    }

    #[test]
    fn auto_trim_single_range_updates_selection_and_trim_range() {
        // With one selected source range, Auto Trim now replaces the selection
        // with the detected active sub-range and mirrors it into `trim_range`.
        let mut harness = harness_with_dynamic_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);

        assert!(harness.state_mut().test_set_selection_frac(0.1, 0.9));
        assert_eq!(harness.state().test_all_selected_ranges().len(), 1);

        assert!(harness.state_mut().test_start_auto_trim());
        wait_for_auto_trim_done(&mut harness);
        harness.run_steps(2);

        let detected = harness
            .state()
            .test_tab_selection()
            .expect("detected primary selection");
        let tab_idx = harness.state().active_tab.expect("active tab");
        assert_eq!(
            harness.state().tabs[tab_idx].trim_range,
            Some(detected),
            "single-section Auto Trim should also update trim_range"
        );
        assert!(
            harness.state().test_tab_extra_selections().is_empty(),
            "single-range Auto Trim should not populate extra_selections"
        );
    }

    #[test]
    fn recording_pause_resume_tracks_state_and_elapsed_pause_time() {
        // The cpal capture stream lives on a worker thread we can't drive in a
        // headless test, so we force the state machine into `Recording` and
        // exercise pause/resume/discard directly — this is exactly the part
        // that used to be UI-only (pause didn't actually stop capture, and
        // there was no resume at all).
        let mut harness = harness_default();
        harness.run_steps(1);

        assert_eq!(harness.state().test_recording_state_name(), "Idle");

        harness.state_mut().test_force_recording_started();
        assert_eq!(harness.state().test_recording_state_name(), "Recording");
        assert!(!harness.state().test_recording_paused_flag());

        harness.state_mut().test_pause_recording();
        assert_eq!(harness.state().test_recording_state_name(), "Paused");
        assert!(
            harness.state().test_recording_paused_flag(),
            "worker should observe the paused flag and stop writing samples"
        );
        assert!(harness.state().test_recording_pause_started());

        // Pausing again (already paused) must be a no-op, not reset the timer.
        harness.state_mut().test_pause_recording();
        assert_eq!(harness.state().test_recording_state_name(), "Paused");

        // Simulate ~2 real seconds having passed while paused.
        harness.state_mut().test_rewind_recording_clock(2.0);
        harness.state_mut().test_resume_recording();

        assert_eq!(harness.state().test_recording_state_name(), "Recording");
        assert!(
            !harness.state().test_recording_paused_flag(),
            "resume should clear the paused flag so the worker writes samples again"
        );
        assert!(!harness.state().test_recording_pause_started());
        let accum = harness.state().test_recording_paused_accum_secs();
        assert!(
            accum >= 1.9,
            "paused_accum should record ~2s of pause time for gapless resume accounting, got {accum}"
        );

        // Resuming again (already recording) must be a no-op.
        harness.state_mut().test_resume_recording();
        assert_eq!(harness.state().test_recording_state_name(), "Recording");

        harness.state_mut().test_discard_recording();
        assert_eq!(
            harness.state().test_recording_state_name(),
            "Finalizing",
            "discard should wait for the capture worker before returning to idle"
        );
        harness.state_mut().test_finish_recording_discard();
        assert_eq!(harness.state().test_recording_state_name(), "Idle");
        assert!(!harness.state().test_recording_paused_flag());
        assert!(!harness.state().test_recording_pause_started());
        assert_eq!(harness.state().test_recording_paused_accum_secs(), 0.0);

        // Pause/resume must be no-ops outside their expected source states.
        harness.state_mut().test_pause_recording();
        assert_eq!(
            harness.state().test_recording_state_name(),
            "Idle",
            "pause should do nothing while idle"
        );
        harness.state_mut().test_resume_recording();
        assert_eq!(
            harness.state().test_recording_state_name(),
            "Idle",
            "resume should do nothing while idle"
        );
    }

    #[test]
    fn recording_tab_stays_open_after_navigating_away() {
        // Opening the Recording tab (mirroring "Tools > Recording...") used to
        // make it vanish from the workspace tab strip the instant you switched
        // to another workspace, because its visibility was tied solely to
        // `workspace_view == Recording`. It should persist like the Effect
        // Graph tab does via `EffectGraphState::workspace_open`.
        let mut harness = harness_default();
        harness.run_steps(1);

        harness.state_mut().test_open_recording_tab();
        harness.run_steps(2);
        assert!(harness.state().test_recording_tab_open());
        harness.get_by_label("[Recording]");

        harness.state_mut().test_switch_to_list_workspace();
        harness.run_steps(2);
        assert!(
            harness.state().test_recording_tab_open(),
            "Recording tab should remain open in the tab strip after navigating away"
        );
        harness.get_by_label("Recording").click();
        harness.run_steps(2);

        assert!(harness.state().test_recording_tab_open());
        harness.get_by_label("[Recording]");
    }

    #[test]
    fn gain_curve_single_click_seeks_without_adding_point() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness.state_mut().test_set_active_tool(ToolKind::Gain));
        assert!(harness.state_mut().test_set_gain_curve(true, &[]));
        harness.run_steps(3);

        let seek_position = editor_canvas_pos_at_frac(&harness, 0.72);
        editor_primary_click_at_pos(&mut harness, seek_position);
        let playhead = harness
            .state()
            .test_playhead_display_now()
            .expect("playhead after gain-curve click");
        let samples_len = harness.state().tabs[harness.state().active_tab.unwrap()].samples_len;
        assert!(
            playhead > samples_len / 2,
            "single-click should seek, got {playhead} of {samples_len}"
        );
        let (_, points_after_click) = harness
            .state()
            .test_gain_curve_state()
            .expect("gain curve state");
        assert!(
            points_after_click.is_empty(),
            "single-click must not add a gain point"
        );
    }

    #[cfg(feature = "kittest_render")]
    #[test]
    fn kittest_render_pitch_curve_before_after_png() {
        let mut harness = harness_with_editor_fixture();
        harness.set_size(egui::vec2(1600.0, 900.0));
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness
            .state_mut()
            .test_set_active_tool(ToolKind::PitchShift));
        assert!(harness.state_mut().test_set_tool_pitch_semitones(3.0));
        harness.run_steps(3);

        let out_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("debug")
            .join("screenshot_verify")
            .join("pitch_curve");
        std::fs::create_dir_all(&out_dir).expect("create pitch curve screenshot dir");
        let before_path = out_dir.join("pitch_static_before.png");
        let after_path = out_dir.join("pitch_curve_after.png");
        let preview_before = first_label_rect(&harness, "Preview");
        let apply_before = first_label_rect(&harness, "Apply");
        let before = harness.render().expect("render static pitch line");
        before
            .save(&before_path)
            .expect("save static pitch screenshot");

        assert!(harness.state_mut().test_set_pitch_curve(
            true,
            &[(0.08, -5.0), (0.35, 7.0), (0.66, -2.5), (0.92, 4.0)],
        ));
        harness.run_steps(3);
        harness.get_by_label("Pitch curve (draw on waveform)");
        harness.get_by_label("4 point(s)");
        let (enabled, points) = harness
            .state()
            .test_pitch_curve_state()
            .expect("pitch curve state");
        assert!(enabled);
        assert_eq!(points.len(), 4);
        assert_rect_nearly_same(
            preview_before,
            first_label_rect(&harness, "Preview"),
            "Pitch Preview while toggling curve",
        );
        assert_rect_nearly_same(
            apply_before,
            first_label_rect(&harness, "Apply"),
            "Pitch Apply while toggling curve",
        );

        let after = harness.render().expect("render pitch curve");
        after
            .save(&after_path)
            .expect("save pitch curve screenshot");
        let changed_pixels = before
            .pixels()
            .zip(after.pixels())
            .filter(|(left, right)| left != right)
            .count();
        assert!(
            changed_pixels > 1_000,
            "pitch curve should visibly replace the static line: {changed_pixels} pixels changed"
        );
    }

    #[cfg(feature = "kittest_render")]
    #[test]
    fn kittest_render_pencil_draft_apply_undo_png() {
        let mut harness = harness_with_editor_fixture();
        harness.set_size(egui::vec2(1600.0, 900.0));
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness.state_mut().test_set_active_tool(ToolKind::Pencil));
        harness.run_steps(3);

        let out_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("debug")
            .join("screenshot_verify")
            .join("pencil_draft");
        std::fs::create_dir_all(&out_dir).expect("create Pencil screenshot dir");
        let before_path = out_dir.join("01_before.png");
        let draft_path = out_dir.join("02_green_draft.png");
        let applied_path = out_dir.join("03_applied.png");
        let undo_path = out_dir.join("04_undo.png");

        harness
            .render()
            .expect("render Pencil baseline")
            .save(&before_path)
            .expect("save Pencil baseline");
        let apply_rect = first_label_rect(&harness, "Apply");
        let cancel_rect = first_label_rect(&harness, "Cancel");

        let tab_idx = harness.state().active_tab.expect("active editor tab");
        let committed_before = harness.state().tabs[tab_idx].ch_samples.clone();
        assert!(harness
            .state_mut()
            .test_pencil_draft_stroke(0.18, 0.9, 0.82, -0.9));
        harness.run_steps(3);
        assert_eq!(
            harness.state().test_preview_overlay_tool(),
            Some(ToolKind::Pencil)
        );
        assert_eq!(
            harness.state().test_preview_audio_tool(),
            Some(ToolKind::Pencil)
        );
        assert!(harness.state().test_playback_source_is_tool_preview());
        assert_eq!(
            harness.state().tabs[tab_idx].ch_samples,
            committed_before,
            "green Pencil draft must not commit early"
        );
        assert_eq!(
            harness.state().tabs[tab_idx].ch_samples_arc.as_ref(),
            &committed_before
        );
        assert_rect_nearly_same(
            apply_rect,
            first_label_rect(&harness, "Apply"),
            "Pencil Apply while draft appears",
        );
        assert_rect_nearly_same(
            cancel_rect,
            first_label_rect(&harness, "Cancel"),
            "Pencil Cancel while draft appears",
        );
        harness
            .render()
            .expect("render green Pencil draft")
            .save(&draft_path)
            .expect("save green Pencil draft");

        // The always-visible Inspector buttons use the same local stroke
        // history as Ctrl+Z/Y and stay in the same position.
        harness.get_by_label("Undo").click();
        harness.run_steps(2);
        {
            let draft = harness.state().tabs[tab_idx]
                .pencil_draft
                .as_ref()
                .expect("Pencil draft after Inspector Undo");
            assert!(draft.undo.is_empty());
            assert_eq!(draft.redo.len(), 1);
        }
        harness.get_by_label("Redo").click();
        harness.run_steps(2);
        {
            let draft = harness.state().tabs[tab_idx]
                .pencil_draft
                .as_ref()
                .expect("Pencil draft after Inspector Redo");
            assert_eq!(draft.undo.len(), 1);
            assert!(draft.redo.is_empty());
        }

        assert!(harness.state_mut().test_pencil_apply_draft());
        harness.run_steps(3);
        assert_eq!(harness.state().test_preview_overlay_tool(), None);
        assert!(harness.state().tabs[tab_idx].pencil_draft.is_none());
        assert_eq!(
            harness.state().tabs[tab_idx].ch_samples_arc.as_ref(),
            &harness.state().tabs[tab_idx].ch_samples
        );
        harness
            .render()
            .expect("render applied Pencil edit")
            .save(&applied_path)
            .expect("save applied Pencil edit");

        assert!(harness.state_mut().test_editor_undo());
        harness.run_steps(3);
        assert_eq!(harness.state().tabs[tab_idx].ch_samples, committed_before);
        harness
            .render()
            .expect("render Pencil global Undo")
            .save(&undo_path)
            .expect("save Pencil global Undo");
    }

    #[test]
    fn pencil_edit_click_move_range_and_ctrl_draw_are_distinct() {
        let mut harness = harness_with_editor_fixture();
        harness.set_size(egui::vec2(1600.0, 900.0));
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness.state_mut().test_set_channel_view_all());
        assert!(harness.state_mut().test_set_tab_view_offset(12_000));
        assert!(harness.state_mut().test_set_tab_samples_per_px(0.125));
        assert!(harness.state_mut().test_set_active_tool(ToolKind::Pencil));
        harness.run_steps(4);
        rightmost_labeled_control(&harness, "Edit").click();
        harness.run_steps(4);

        let tab_idx = harness.state().active_tab.expect("active Pencil tab");
        let lane_count = harness.state().tabs[tab_idx].ch_samples.len().max(1);
        let channel = 0;

        #[cfg(feature = "kittest_render")]
        let out_dir = {
            let out_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("debug")
                .join("screenshot_verify")
                .join("pencil_interactions");
            std::fs::create_dir_all(&out_dir).expect("create Pencil interaction screenshot dir");
            harness
                .render()
                .expect("render Pencil interaction baseline")
                .save(out_dir.join("01_before.png"))
                .expect("save Pencil interaction baseline");
            out_dir
        };

        // A simple point click selects only: no sample or local Undo mutation.
        let click_sample = 12_025;
        let before_click = harness.state().tabs[tab_idx]
            .preview_overlay
            .as_ref()
            .expect("Pencil overlay before click")
            .channels[channel]
            .clone();
        let undo_before_click = harness.state().tabs[tab_idx]
            .pencil_draft
            .as_ref()
            .expect("Pencil draft before click")
            .undo
            .len();
        let click_pos =
            editor_pencil_point_pos(&harness, click_sample, channel, channel, lane_count);
        editor_primary_click_at_pos(&mut harness, click_pos);
        let tab = &harness.state().tabs[tab_idx];
        assert_eq!(
            tab.preview_overlay.as_ref().unwrap().channels[channel],
            before_click,
            "plain point click must not draw or alter samples"
        );
        let draft = tab.pencil_draft.as_ref().unwrap();
        assert_eq!(draft.undo.len(), undo_before_click);
        let selection = draft.selection.as_ref().expect("clicked point selection");
        assert_eq!(
            (selection.start, selection.end),
            (click_sample, click_sample + 1)
        );
        assert_eq!(selection.channels, vec![channel]);

        // Dragging the selected point changes that point vertically and
        // records exactly one draft operation.
        let point_move_start =
            editor_pencil_point_pos(&harness, click_sample, channel, channel, lane_count);
        let point_move_end = point_move_start - egui::vec2(0.0, 28.0);
        editor_pointer_drag(&mut harness, point_move_start, point_move_end);
        let tab = &harness.state().tabs[tab_idx];
        let after_point_move = &tab.preview_overlay.as_ref().unwrap().channels[channel];
        assert!(
            (after_point_move[click_sample] - before_click[click_sample]).abs() > 0.01,
            "plain point drag should move the grabbed point"
        );
        assert_eq!(
            after_point_move[click_sample - 1],
            before_click[click_sample - 1],
            "point drag must not move its left neighbor"
        );
        assert_eq!(
            after_point_move[click_sample + 1],
            before_click[click_sample + 1],
            "point drag must not move its right neighbor"
        );
        assert_eq!(
            tab.pencil_draft.as_ref().unwrap().undo.len(),
            undo_before_click + 1
        );

        // Dragging away from the curve selects a horizontal sample range but
        // does not modify audio or create Undo history.
        let range_start = 12_038;
        let range_end = 12_045;
        let lane = editor_wave_lane_rect(&harness, channel, lane_count);
        let range_anchor_point =
            editor_pencil_point_pos(&harness, range_start, channel, channel, lane_count);
        let empty_y = if range_anchor_point.y < lane.center().y {
            (range_anchor_point.y + 28.0).min(lane.bottom() - 12.0)
        } else {
            (range_anchor_point.y - 28.0).max(lane.top() + 12.0)
        };
        let range_drag_start = egui::pos2(range_anchor_point.x, empty_y);
        let range_drag_end = egui::pos2(
            editor_pencil_point_pos(&harness, range_end, channel, channel, lane_count).x,
            empty_y,
        );
        let before_range_select = harness.state().tabs[tab_idx]
            .preview_overlay
            .as_ref()
            .unwrap()
            .channels[channel]
            .clone();
        let undo_before_range_select = harness.state().tabs[tab_idx]
            .pencil_draft
            .as_ref()
            .unwrap()
            .undo
            .len();
        editor_pointer_drag(&mut harness, range_drag_start, range_drag_end);
        let tab = &harness.state().tabs[tab_idx];
        assert_eq!(
            tab.preview_overlay.as_ref().unwrap().channels[channel],
            before_range_select,
            "range selection must not alter samples"
        );
        let draft = tab.pencil_draft.as_ref().unwrap();
        assert_eq!(draft.undo.len(), undo_before_range_select);
        let selection = draft.selection.as_ref().expect("range point selection");
        assert_eq!(
            (selection.start, selection.end),
            (range_start, range_end + 1)
        );
        assert_eq!(selection.channels, vec![channel]);

        #[cfg(feature = "kittest_render")]
        harness
            .render()
            .expect("render selected Pencil point range")
            .save(out_dir.join("02_range_selected.png"))
            .expect("save selected Pencil point range");

        // Dragging any selected point moves every selected point by the same
        // vertical delta while keeping the sample-time positions fixed.
        let group_grab_sample = range_start + 2;
        let group_move_start =
            editor_pencil_point_pos(&harness, group_grab_sample, channel, channel, lane_count);
        let group_move_end = group_move_start - egui::vec2(0.0, 24.0);
        editor_pointer_drag(&mut harness, group_move_start, group_move_end);
        let tab = &harness.state().tabs[tab_idx];
        let after_group_move = &tab.preview_overlay.as_ref().unwrap().channels[channel];
        let first_delta = after_group_move[range_start] - before_range_select[range_start];
        assert!(
            first_delta.abs() > 0.01,
            "selected range should move vertically"
        );
        for sample in range_start..=range_end {
            let delta = after_group_move[sample] - before_range_select[sample];
            assert!(
                (delta - first_delta).abs() <= 1.0e-5,
                "selected samples must keep shape and timing: sample={sample} delta={delta} expected={first_delta}"
            );
        }
        assert_eq!(
            after_group_move[range_start - 1],
            before_range_select[range_start - 1]
        );
        assert_eq!(
            after_group_move[range_end + 1],
            before_range_select[range_end + 1]
        );
        assert_eq!(
            tab.pencil_draft.as_ref().unwrap().undo.len(),
            undo_before_range_select + 1
        );

        #[cfg(feature = "kittest_render")]
        harness
            .render()
            .expect("render vertically moved Pencil range")
            .save(out_dir.join("03_range_moved.png"))
            .expect("save vertically moved Pencil range");

        // Freehand interpolation is exclusive to Ctrl+drag.
        let draw_start_sample = 12_054;
        let draw_end_sample = 12_062;
        let draw_lane = editor_wave_lane_rect(&harness, channel, lane_count);
        let draw_start = egui::pos2(
            editor_pencil_point_pos(&harness, draw_start_sample, channel, channel, lane_count).x,
            draw_lane.center().y + 52.0,
        );
        let draw_end = egui::pos2(
            editor_pencil_point_pos(&harness, draw_end_sample, channel, channel, lane_count).x,
            draw_lane.center().y - 52.0,
        );
        let before_ctrl_draw = harness.state().tabs[tab_idx]
            .preview_overlay
            .as_ref()
            .unwrap()
            .channels[channel]
            .clone();
        let undo_before_ctrl_draw = harness.state().tabs[tab_idx]
            .pencil_draft
            .as_ref()
            .unwrap()
            .undo
            .len();
        editor_pointer_drag_with_modifiers(&mut harness, draw_start, draw_end, Modifiers::CTRL);
        let tab = &harness.state().tabs[tab_idx];
        let after_ctrl_draw = &tab.preview_overlay.as_ref().unwrap().channels[channel];
        let changed = (draw_start_sample..=draw_end_sample)
            .filter(|&sample| (after_ctrl_draw[sample] - before_ctrl_draw[sample]).abs() > 0.001)
            .count();
        assert!(
            changed >= 4,
            "Ctrl+drag should draw an interpolated line across multiple samples: changed={changed}"
        );
        assert_eq!(
            tab.pencil_draft.as_ref().unwrap().undo.len(),
            undo_before_ctrl_draw + 1
        );

        #[cfg(feature = "kittest_render")]
        harness
            .render()
            .expect("render Ctrl-drawn Pencil line")
            .save(out_dir.join("04_ctrl_draw.png"))
            .expect("save Ctrl-drawn Pencil line");
    }

    #[cfg(feature = "kittest_render")]
    #[test]
    fn kittest_render_max_zoom_sample_points_png() {
        let mut harness = harness_with_editor_fixture();
        harness.set_size(egui::vec2(1600.0, 900.0));
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness.state_mut().test_set_channel_view_all());
        assert!(harness.state_mut().test_set_tab_view_offset(12_000));
        assert!(harness.state_mut().test_set_tab_samples_per_px(0.125));
        harness.run_steps(4);

        let out_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("debug")
            .join("screenshot_verify")
            .join("sample_points");
        std::fs::create_dir_all(&out_dir).expect("create sample-point screenshot dir");
        harness
            .render()
            .expect("render maximum zoom with sample points")
            .save(out_dir.join("02_after_point_line.png"))
            .expect("save maximum zoom with sample points");

        assert!(harness.state_mut().test_set_active_tool(ToolKind::Pencil));
        harness.run_steps(3);
        harness
            .render()
            .expect("render Pencil before explicit Edit")
            .save(out_dir.join("04_pencil_before_edit.png"))
            .expect("save Pencil before explicit Edit");
        assert!(harness.state().tabs[harness.state().active_tab.unwrap()]
            .pencil_draft
            .is_none());
        rightmost_labeled_control(&harness, "Edit").click();
        harness.run_steps(3);
        let tab_idx = harness.state().active_tab.unwrap();
        assert!(harness.state().tabs[tab_idx].pencil_draft.is_some());
        assert_eq!(
            harness.state().test_preview_overlay_tool(),
            Some(ToolKind::Pencil)
        );
        assert_eq!(
            harness.state().tabs[tab_idx]
                .preview_overlay
                .as_ref()
                .map(|overlay| &overlay.channels),
            Some(&harness.state().tabs[tab_idx].ch_samples)
        );
        harness
            .render()
            .expect("render aligned unchanged Pencil draft")
            .save(out_dir.join("05_edit_aligned.png"))
            .expect("save aligned unchanged Pencil draft");

        let sample_count = harness.state().test_tab_samples_len().max(1) as f32;
        let from_frac = 12_010.0 / sample_count;
        let to_frac = 12_140.0 / sample_count;
        assert!(harness
            .state_mut()
            .test_pencil_draft_stroke(from_frac, 0.85, to_frac, -0.85));
        harness.run_steps(3);
        assert_eq!(
            harness.state().test_preview_overlay_tool(),
            Some(ToolKind::Pencil)
        );
        harness
            .render()
            .expect("render green Pencil sample points")
            .save(out_dir.join("06_edited_point_line.png"))
            .expect("save green Pencil sample points");

        rightmost_labeled_control(&harness, "Reset").click();
        harness.run_steps(3);
        let tab = &harness.state().tabs[tab_idx];
        let draft = tab.pencil_draft.as_ref().expect("Edit mode after Reset");
        assert!(draft.undo.is_empty());
        assert!(draft.redo.is_empty());
        assert_eq!(
            tab.preview_overlay
                .as_ref()
                .map(|overlay| &overlay.channels),
            Some(&tab.ch_samples)
        );
        harness
            .render()
            .expect("render reset aligned Pencil draft")
            .save(out_dir.join("07_reset_aligned.png"))
            .expect("save reset aligned Pencil draft");
    }

    /// The list hand-rolls its vertical virtualization: `TableBuilder` is built
    /// with `vscroll(false)` and only the visible row window is handed to it.
    /// The window size used to be computed as `(avail_h - header_h) / row_h`,
    /// ignoring the `item_spacing.y` that `TableBody::rows` actually adds
    /// between rows. That over-counted how many rows fit, so the scroll clamp
    /// `total - visible` stopped short and the last few rows were laid out
    /// below the clip rect with no inner scroll able to reveal them.
    ///
    /// Asserting on `test_list_scroll_row` cannot catch this -- the rows *were*
    /// in the rendered window. Only the painted rect proves it.
    #[test]
    fn list_tail_is_reachable_at_max_scroll() {
        let mut harness = harness_with_startup(StartupConfig {
            dummy_list_count: Some(400),
            ..StartupConfig::default()
        });
        harness.run_steps(3);
        let last = harness.state().test_files_len() - 1;
        assert_eq!(last, 399);

        harness.state_mut().test_list_scroll_to_end();
        harness.run_steps(2);

        assert_eq!(
            harness.state().test_list_last_fully_visible_row(),
            Some(last),
            "last row not fully on screen at max scroll (scroll_row={})",
            harness.state().test_list_scroll_row()
        );
    }

    /// End selects the final row and the auto-scroll centers it, clamped to the
    /// same maximum. With the inflated window size the selection landed in a
    /// window slot that was painted off-screen: the row was selected but
    /// invisible.
    #[test]
    fn end_key_puts_the_last_row_fully_on_screen() {
        let mut harness = harness_with_startup(StartupConfig {
            dummy_list_count: Some(400),
            ..StartupConfig::default()
        });
        harness.run_steps(3);
        let last = harness.state().test_files_len() - 1;

        assert!(harness.state_mut().test_select_row_with_autoscroll(last));
        harness.run_steps(3);

        assert_eq!(
            harness.state().test_list_last_fully_visible_row(),
            Some(last),
            "End-selected last row not fully on screen (scroll_row={})",
            harness.state().test_list_scroll_row()
        );
    }

    /// The user's report, reproduced literally: keep turning the wheel over the
    /// list and the tail must arrive. This also covers the pixels-to-rows
    /// conversion, which divided by the row height instead of the row pitch and
    /// so under-scrolled by `spacing_y / row_h` per notch.
    #[test]
    fn wheel_scrolling_reaches_the_last_row() {
        let mut harness = harness_with_startup(StartupConfig {
            dummy_list_count: Some(300),
            ..StartupConfig::default()
        });
        harness.run_steps(3);
        let last = harness.state().test_files_len() - 1;

        // The wheel handler only runs while the pointer is over the list and
        // the list owns the scroll surface, so park the pointer there first.
        harness.hover_at(egui::pos2(640.0, 200.0));
        harness.run_steps(1);

        let mut settled = 0;
        for _ in 0..400 {
            let before = harness.state().test_list_scroll_row();
            harness.event(egui::Event::MouseWheel {
                unit: MouseWheelUnit::Line,
                delta: egui::vec2(0.0, -8.0),
                phase: egui::TouchPhase::Move,
                modifiers: Modifiers::default(),
            });
            harness.run_steps(1);
            if harness.state().test_list_scroll_row() == before {
                settled += 1;
                if settled >= 5 {
                    break;
                }
            } else {
                settled = 0;
            }
        }

        assert_eq!(
            harness.state().test_list_last_fully_visible_row(),
            Some(last),
            "wheel scrolling never reached the last row (scroll_row={})",
            harness.state().test_list_scroll_row()
        );
    }

    /// The list note is edited inline in the list and stored in the session,
    /// but the top search box never looked at it.
    #[test]
    fn search_matches_the_list_note() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        let total = harness.state().files.len();
        assert!(total >= 3, "need a few rows, got {total}");

        harness.state_mut().items[1].note = "retake with a longer tail".to_string();
        harness.state_mut().test_set_search_query("longer tail");
        harness.run_steps(2);

        let matched = harness.state().test_visible_list_paths();
        assert_eq!(
            matched.len(),
            1,
            "only the noted row should match, got {matched:?}"
        );
        assert_eq!(matched[0], harness.state().items[1].path);
    }

    #[test]
    fn search_matches_the_list_note_with_regex() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        assert!(harness.state().files.len() >= 3);

        harness.state_mut().items[1].note = "retake at 120bpm".to_string();
        harness.state_mut().test_set_search_use_regex(true);
        harness.state_mut().test_set_search_query(r"\d+bpm");
        harness.run_steps(2);

        let matched = harness.state().test_visible_list_paths();
        assert_eq!(matched.len(), 1, "got {matched:?}");
        assert_eq!(matched[0], harness.state().items[1].path);
    }

    /// A list above `list_sync_threshold()` filters in per-frame slices via
    /// `FilterJob` instead of one synchronous pass. Both paths route through the
    /// same predicate, and this pins that down: a note-only match must survive
    /// the sliced path too.
    #[test]
    fn sliced_filter_job_matches_the_note_too() {
        let mut harness = harness_with_startup(StartupConfig {
            // Above the highest `list_sync_threshold()` (50k) so the sliced
            // path is taken on every performance tier.
            dummy_list_count: Some(60_000),
            ..StartupConfig::default()
        });
        harness.run_steps(2);
        assert_eq!(harness.state().test_files_len(), 60_000);

        harness.state_mut().items[42].note = "zzq-unique-note-marker".to_string();
        let expected = harness.state().items[42].path.clone();

        harness
            .state_mut()
            .test_apply_search_via_jobs("zzq-unique-note-marker");
        assert!(
            harness.state().test_sort_job_active(),
            "60k rows should have taken the sliced filter path"
        );

        let mut frames = 0;
        while harness.state().test_sort_job_active() {
            harness.step();
            frames += 1;
            assert!(frames < 20_000, "sliced filter never settled");
        }

        let matched = harness.state().test_visible_list_paths();
        assert_eq!(matched.len(), 1, "got {} matches", matched.len());
        assert_eq!(matched[0], expected);
    }

    /// "How many files are loaded" outranks "is the waveform drawn". The row
    /// waveform needs a full file decode per visible row, and with the Wave
    /// column on by default those decodes were queued from the first frame of a
    /// folder load, competing with the walker for the same disk and worker
    /// pool. They must now wait until every row is listed.
    ///
    /// A real scan of a small folder finishes inside one frame, so the scanning
    /// state is held explicitly rather than raced.
    #[test]
    fn a_running_scan_queues_no_full_decodes() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);

        // Both `Header` and `Decode` read the whole file (Header does the
        // header pass and the decode in one task); only `HeaderOnly` is cheap.
        let decoding_tasks = |h: &Harness<'static, WavesPreviewer>| {
            let (_, header, decode) = h.state().test_meta_task_counts();
            header + decode
        };

        // Baseline: with the scan finished, visible rows do ask for a decode.
        let mut frames = 0;
        while decoding_tasks(&harness) == 0 {
            harness.run_steps(1);
            frames += 1;
            assert!(frames < 600, "no decode was ever queued on an idle list");
        }
        assert!(!harness.state().test_list_meta_detail_is_header_only());

        // Now hold the list in the scanning state.
        harness.state_mut().test_force_scan_in_progress(true);
        harness.run_steps(1);
        assert!(
            harness.state().test_list_meta_detail_is_header_only(),
            "a live scan must withhold the thumb"
        );
        let during_scan = decoding_tasks(&harness);
        harness.run_steps(30);
        assert_eq!(
            decoding_tasks(&harness),
            during_scan,
            "no file decode may be queued while the scan is listing rows"
        );

        // ...and the thumbs resume once it finishes, because
        // `queue_full_meta_for_path` re-queues a row that only has header data.
        harness.state_mut().test_force_scan_in_progress(false);
        assert!(!harness.state().test_list_meta_detail_is_header_only());
    }

    /// Clicking partway along a row's waveform must start playback from that
    /// point. A plain wav row reaches the whole-file streaming transport (which
    /// `select_and_load` activates when Auto Play is on), so there is nothing
    /// to wait for and the seek applies immediately.
    #[test]
    fn list_wave_seek_moves_the_playhead_on_a_wav_row() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        harness.state_mut().test_set_auto_play_list_nav(true);

        // Pick a wav row long enough that half of it is unambiguous.
        let row = wait_for_seekable_row(&mut harness, 0.5, Some("wav"));
        let duration = harness.state().test_row_duration_secs(row).unwrap();

        harness.state_mut().test_select_row_with_autoscroll(row);
        harness.run_steps(4);

        harness.state_mut().test_list_seek_row_frac(row, 0.5);
        harness.run_steps(2);

        assert_eq!(
            harness.state().test_list_seek_pending_frac(),
            None,
            "a whole-file transport should not need to park the seek"
        );
        let at = harness
            .state()
            .test_playback_source_time_sec()
            .expect("playback position");
        let want = duration * 0.5;
        assert!(
            (at - want).abs() < duration * 0.1,
            "expected ~{want:.3}s, got {at:.3}s (duration {duration:.3}s)"
        );
    }

    /// An mp3 row plays from a decoded buffer that starts as a ~1.2s prefix, so
    /// a seek near the end of the file lands beyond what has been decoded. It
    /// must be parked (not clamped to the prefix, which would silently play the
    /// wrong part) and applied once the decode reaches it.
    #[test]
    fn list_wave_seek_past_the_prefix_is_parked_then_applied() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("test_samples/bgms");
        if !dir.is_dir() {
            eprintln!("skipping: {} not present", dir.display());
            return;
        }
        let mut cfg = StartupConfig::default();
        cfg.open_folder = Some(dir);
        let mut harness = harness_with_startup(cfg);
        wait_for_scan(&mut harness);

        let row = wait_for_seekable_row(&mut harness, 10.0, Some("mp3"));
        let duration = harness.state().test_row_duration_secs(row).unwrap();

        harness.state_mut().test_select_row_with_autoscroll(row);
        harness.run_steps(4);

        harness.state_mut().test_list_seek_row_frac(row, 0.8);
        let parked = harness.state().test_list_seek_pending_frac();
        assert_eq!(
            parked,
            Some(0.8),
            "a seek past the decoded prefix must be parked, not clamped"
        );

        // The decode is progressive; the parked seek retires when it arrives.
        let start = Instant::now();
        while harness.state().test_list_seek_pending_frac().is_some() {
            harness.run_steps(1);
            assert!(
                start.elapsed() < Duration::from_secs(60),
                "parked seek never retired"
            );
        }

        let at = harness
            .state()
            .test_playback_source_time_sec()
            .expect("playback position");
        let want = duration * 0.8;
        assert!(
            (at - want).abs() < duration * 0.1,
            "expected ~{want:.1}s, got {at:.1}s (duration {duration:.1}s)"
        );
    }

    /// A waveform click always parks the position, but it must not override the
    /// user's transport preferences: with Auto Play off and nothing already
    /// sounding, the position is set and Space is left to start playback.
    #[test]
    fn list_wave_seek_does_not_start_playback_when_auto_play_is_off() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        harness.state_mut().test_set_auto_play_list_nav(false);
        assert!(!harness.state().test_auto_play_list_nav());

        let row = wait_for_seekable_row(&mut harness, 0.5, None);

        harness.state_mut().test_select_row_with_autoscroll(row);
        harness.run_steps(4);
        harness.state_mut().test_list_seek_row_frac(row, 0.5);
        harness.run_steps(4);

        // With Auto Play off the preview decodes without emitting partials, so
        // the seek is parked until the buffer lands. Let it retire.
        let start = Instant::now();
        while harness.state().test_list_seek_pending_frac().is_some() {
            harness.run_steps(1);
            assert!(
                start.elapsed() < Duration::from_secs(30),
                "parked seek never retired"
            );
        }

        assert!(
            !harness.state().test_audio_is_playing(),
            "a waveform click must not start playback when Auto Play is off"
        );
        // ...but the position is armed and the list holds focus, so Space works.
        let at = harness
            .state()
            .test_playback_source_time_sec()
            .expect("position should be armed even though nothing is playing");
        assert!(at > 0.0, "seek position was not armed: {at}");
        assert!(
            harness.state().test_list_has_focus(),
            "a waveform click must leave the list focused so Space can play"
        );
    }

    #[test]
    fn list_wave_seek_pointer_hold_is_silent_until_release() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        harness.state_mut().test_set_auto_play_list_nav(true);
        let row = wait_for_seekable_row(&mut harness, 0.5, Some("wav"));
        let duration = harness.state().test_row_duration_secs(row).unwrap();
        harness.state_mut().test_select_row_with_autoscroll(row);
        harness.run_steps(4);

        let rect = harness.get_by_label(&format!("List seek row {row}")).rect();
        let start = egui::pos2(rect.left() + rect.width() * 0.25, rect.center().y);
        let end = egui::pos2(rect.left() + rect.width() * 0.70, rect.center().y);
        harness.hover_at(start);
        harness.event_modifiers(
            egui::Event::PointerButton {
                pos: start,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
            Modifiers::NONE,
        );
        harness.run_steps(2);
        assert!(!harness.state().test_audio_is_playing());
        assert!(
            harness.state().test_list_seek_gesture_frac().is_some(),
            "pointer-down should create a held seek without committing it"
        );
        let held_source_time = harness.state().test_playback_source_time_sec();

        harness.event_modifiers(egui::Event::PointerMoved(end), Modifiers::NONE);
        harness.run_steps(2);
        assert!(!harness.state().test_audio_is_playing());
        let held_frac = harness
            .state()
            .test_list_seek_gesture_frac()
            .expect("held seek fraction");
        assert!((held_frac - 0.70).abs() < 0.08, "held frac={held_frac}");
        assert_eq!(
            harness.state().test_playback_source_time_sec(),
            held_source_time,
            "dragging must not move the real transport"
        );

        harness.event_modifiers(
            egui::Event::PointerButton {
                pos: end,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: Modifiers::NONE,
            },
            Modifiers::NONE,
        );
        harness.run_steps(3);
        assert!(harness.state().test_audio_is_playing());
        assert_eq!(harness.state().test_list_seek_gesture_frac(), None);
        let committed = harness
            .state()
            .test_playback_source_time_sec()
            .expect("committed source time");
        assert!(
            (committed - duration * 0.70).abs() < duration * 0.1,
            "release should commit near 70%: {committed:.3}/{duration:.3}"
        );
    }

    #[test]
    fn list_stop_then_play_reuses_position_until_another_item_is_selected() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        harness.state_mut().test_set_auto_play_list_nav(true);
        harness
            .state_mut()
            .test_set_list_stop_returns_to_start(false);
        let row = wait_for_seekable_row(&mut harness, 0.5, Some("wav"));
        harness.state_mut().test_select_row_with_autoscroll(row);
        harness.run_steps(3);
        harness.state_mut().test_list_seek_row_frac(row, 0.4);
        harness.run_steps(2);
        assert!(harness.state().test_audio_is_playing());

        harness.state_mut().test_request_workspace_play_toggle();
        let stopped_at = harness
            .state()
            .test_playback_source_time_sec()
            .expect("stopped source time");
        assert!(!harness.state().test_audio_is_playing());
        harness.state_mut().test_request_workspace_play_toggle();
        let resumed_at = harness
            .state()
            .test_playback_source_time_sec()
            .expect("resumed source time");
        assert!(harness.state().test_audio_is_playing());
        assert!(
            (resumed_at - stopped_at).abs() < 1e-3,
            "same-item resume reset position: stopped={stopped_at} resumed={resumed_at}"
        );

        let other = if row == 0 { 1 } else { 0 };
        harness.state_mut().test_select_row_with_autoscroll(other);
        harness.run_steps(3);
        let reset_at = harness
            .state()
            .test_playback_source_time_sec()
            .unwrap_or(0.0);
        assert!(
            reset_at < 0.05,
            "new item should reset to its start: {reset_at}"
        );
    }

    #[test]
    fn list_stop_returns_to_start_when_preference_is_enabled() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        harness.state_mut().test_set_auto_play_list_nav(true);
        harness
            .state_mut()
            .test_set_list_stop_returns_to_start(true);
        let row = wait_for_seekable_row(&mut harness, 0.5, Some("wav"));
        harness.state_mut().test_select_row_with_autoscroll(row);
        harness.run_steps(3);
        harness.state_mut().test_list_seek_row_frac(row, 0.4);
        harness.run_steps(2);
        assert!(harness.state().test_audio_is_playing());

        harness.state_mut().test_request_workspace_play_toggle();
        assert!(!harness.state().test_audio_is_playing());
        let stopped_at = harness
            .state()
            .test_playback_source_time_sec()
            .expect("rewound source time");
        assert!(
            stopped_at < 0.01,
            "enabled stop preference should rewind to 0:00, got {stopped_at}"
        );

        harness.state_mut().test_request_workspace_play_toggle();
        assert!(harness.state().test_audio_is_playing());
        let resumed_at = harness
            .state()
            .test_playback_source_time_sec()
            .expect("restarted source time");
        assert!(
            resumed_at < 0.01,
            "the next Play should begin at 0:00, got {resumed_at}"
        );
    }

    #[test]
    fn slow_progressive_decode_rebuffers_before_reaching_the_buffer_end() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        harness.state_mut().test_set_auto_play_list_nav(false);
        let row = wait_for_seekable_row(&mut harness, 0.5, Some("wav"));
        harness.state_mut().test_select_row_with_autoscroll(row);
        let deadline = Instant::now() + Duration::from_secs(30);
        while !harness.state().test_audio_has_samples() {
            harness.run_steps(1);
            assert!(Instant::now() < deadline, "List buffer did not load");
        }
        let len = harness.state().test_audio_source_len();
        assert!(len > 100);

        harness
            .state_mut()
            .test_force_list_decode_progress(10.0, Some(10.0), 0.1, true);
        harness.state_mut().test_request_workspace_play_toggle();
        assert!(harness.state().test_audio_is_playing());
        let buffered_ahead = (len / 10).max(32);
        harness
            .state_mut()
            .test_audio_seek_to_sample(len.saturating_sub(buffered_ahead));
        harness
            .state_mut()
            .test_force_list_decode_progress(0.5, Some(0.5), 1.0, false);
        harness.state_mut().test_maintain_list_playback_buffer();
        assert!(
            !harness.state().test_audio_is_playing(),
            "low-water guard must stop before callback underrun"
        );
        assert!(harness.state().test_list_seek_pending_frac().is_some());

        harness
            .state_mut()
            .test_force_list_decode_progress(10.0, Some(10.0), 0.1, true);
        harness.state_mut().test_apply_pending_list_seek();
        assert!(harness.state().test_audio_is_playing());
        assert_eq!(harness.state().test_list_seek_pending_frac(), None);
    }

    #[test]
    fn opening_editor_from_playing_list_wav_preserves_transport() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        harness.state_mut().test_set_auto_play_list_nav(true);
        let row = wait_for_seekable_row(&mut harness, 0.5, Some("wav"));
        let path = path_for_row(harness.state(), row);
        harness.state_mut().test_select_row_with_autoscroll(row);
        harness.run_steps(3);
        harness.state_mut().test_list_seek_row_frac(row, 0.35);
        harness.run_steps(2);
        let before = harness
            .state()
            .test_playback_source_time_sec()
            .expect("list source time");
        assert!(harness.state().test_audio_is_playing());

        assert!(harness.state_mut().test_open_tab_for_path(&path));
        assert!(harness.state().test_audio_is_playing());
        let immediately_after = harness
            .state()
            .test_playback_source_time_sec()
            .expect("handoff source time");
        assert!((immediately_after - before).abs() < 1e-3);
        assert_eq!(
            harness.state().test_editor_playback_handoff(),
            Some((path.clone(), true))
        );

        harness.run_steps(5);
        assert!(harness.state().test_is_editor_workspace_active());
        assert!(harness.state().test_audio_is_playing());
        assert!(harness.state().test_playback_source_is_editor_path(&path));
        assert_eq!(harness.state().test_editor_playback_handoff(), None);
        let after = harness
            .state()
            .test_playback_source_time_sec()
            .expect("editor source time");
        assert!(
            (after - before).abs() < 0.02,
            "Editor handoff moved position: before={before} after={after}"
        );
    }

    #[test]
    fn opening_editor_from_playing_compressed_list_keeps_audio_during_decode() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("test_samples/bgms");
        if !dir.is_dir() {
            eprintln!("skipping: {} not present", dir.display());
            return;
        }
        let mut cfg = StartupConfig::default();
        cfg.open_folder = Some(dir);
        let mut harness = harness_with_startup(cfg);
        wait_for_scan(&mut harness);
        harness.state_mut().test_set_auto_play_list_nav(true);
        let row = wait_for_seekable_row(&mut harness, 2.0, Some("mp3"));
        let path = path_for_row(harness.state(), row);
        harness.state_mut().test_select_row_with_autoscroll(row);
        let deadline = Instant::now() + Duration::from_secs(30);
        while !harness.state().test_audio_is_playing() {
            harness.run_steps(1);
            assert!(
                Instant::now() < deadline,
                "compressed List preview did not start"
            );
        }
        let before = harness
            .state()
            .test_playback_source_time_sec()
            .expect("compressed List source time");

        assert!(harness.state_mut().test_open_tab_for_path(&path));
        assert!(
            harness.state().test_audio_is_playing(),
            "opening the loading Editor must not stop the List transport"
        );
        assert_eq!(
            harness.state().test_editor_playback_handoff(),
            Some((path.clone(), true))
        );
        let immediate = harness
            .state()
            .test_playback_source_time_sec()
            .expect("compressed handoff source time");
        assert!((immediate - before).abs() < 1e-3);

        harness.state_mut().test_request_workspace_play_toggle();
        assert!(!harness.state().test_audio_is_playing());
        assert_eq!(
            harness.state().test_editor_playback_handoff(),
            Some((path.clone(), false))
        );
        harness.state_mut().test_request_workspace_play_toggle();
        assert!(harness.state().test_audio_is_playing());
        assert_eq!(
            harness.state().test_editor_playback_handoff(),
            Some((path.clone(), true))
        );

        wait_for_tab_fully_loaded(&mut harness);
        harness.run_steps(3);
        assert!(harness.state().test_audio_is_playing());
        assert!(harness.state().test_playback_source_is_editor_path(&path));
        assert_eq!(harness.state().test_editor_playback_handoff(), None);
        let after = harness
            .state()
            .test_playback_source_time_sec()
            .expect("compressed Editor source time");
        assert!((after - before).abs() < 0.05);
    }

    #[test]
    fn opening_editor_from_stopped_list_keeps_position_and_stopped_state() {
        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        harness.state_mut().test_set_auto_play_list_nav(false);
        let row = wait_for_seekable_row(&mut harness, 0.5, Some("wav"));
        let path = path_for_row(harness.state(), row);
        harness.state_mut().test_select_row_with_autoscroll(row);
        harness.run_steps(3);
        harness.state_mut().test_list_seek_row_frac(row, 0.55);
        let seek_deadline = Instant::now() + Duration::from_secs(30);
        while harness.state().test_list_seek_pending_frac().is_some() {
            harness.run_steps(1);
            assert!(
                Instant::now() < seek_deadline,
                "stopped List seek did not finish decoding"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        harness.run_steps(2);
        assert!(!harness.state().test_audio_is_playing());
        let before = harness
            .state()
            .test_playback_source_time_sec()
            .expect("stopped List source time");

        assert!(harness.state_mut().test_open_tab_for_path(&path));
        harness.run_steps(5);
        assert!(harness.state().test_is_editor_workspace_active());
        assert!(!harness.state().test_audio_is_playing());
        let after = harness
            .state()
            .test_playback_source_time_sec()
            .expect("stopped Editor source time");
        assert!(
            (after - before).abs() < 0.02,
            "stopped Editor handoff moved from {before:.6}s to {after:.6}s"
        );
    }

    #[cfg(feature = "kittest_render")]
    #[test]
    fn kittest_render_list_seek_editor_handoff_sequence() {
        let out_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("debug/screenshot_verify/list_seek_editor_handoff");
        std::fs::create_dir_all(&out_dir).expect("create handoff screenshot dir");
        let mut harness = harness_with_wavs(false);
        harness.set_size(egui::vec2(1280.0, 760.0));
        wait_for_scan(&mut harness);
        harness.state_mut().test_set_auto_play_list_nav(true);
        let row = wait_for_seekable_row(&mut harness, 0.5, Some("wav"));
        let path = path_for_row(harness.state(), row);
        harness.state_mut().test_select_row_with_autoscroll(row);
        harness.run_steps(4);
        harness.state_mut().test_list_seek_row_frac(row, 0.20);
        harness.run_steps(2);

        let before = harness.render().expect("render before seek");
        before
            .save(out_dir.join("01_before_seek.png"))
            .expect("save before seek");

        let rect = harness.get_by_label(&format!("List seek row {row}")).rect();
        let target = egui::pos2(rect.left() + rect.width() * 0.68, rect.center().y);
        harness.hover_at(target);
        harness.event_modifiers(
            egui::Event::PointerButton {
                pos: target,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
            Modifiers::NONE,
        );
        harness.run_steps(2);
        assert!(!harness.state().test_audio_is_playing());
        let held = harness.render().expect("render held seek");
        held.save(out_dir.join("02_seek_held.png"))
            .expect("save held seek");

        harness.event_modifiers(
            egui::Event::PointerButton {
                pos: target,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: Modifiers::NONE,
            },
            Modifiers::NONE,
        );
        harness.run_steps(3);
        assert!(harness.state().test_audio_is_playing());
        let released = harness.render().expect("render released seek");
        released
            .save(out_dir.join("03_seek_released.png"))
            .expect("save released seek");

        let before_editor_time = harness
            .state()
            .test_playback_source_time_sec()
            .expect("before editor source time");
        assert!(harness.state_mut().test_open_tab_for_path(&path));
        harness.run_steps(5);
        assert!(harness.state().test_audio_is_playing());
        let after_editor_time = harness
            .state()
            .test_playback_source_time_sec()
            .expect("after editor source time");
        assert!((after_editor_time - before_editor_time).abs() < 0.02);
        let editor = harness.render().expect("render editor handoff");
        editor
            .save(out_dir.join("04_editor_handoff.png"))
            .expect("save editor handoff");
        eprintln!("[shot] wrote {}", out_dir.display());
    }

    /// Not an assertion test: renders the list with a seek in progress so the
    /// playhead, the progress fill and the undecoded shading can be eyeballed
    /// at the real row height. Run with:
    ///   cargo test --features kittest_render -- --ignored seek_bar_screenshot --nocapture
    #[cfg(feature = "kittest_render")]
    #[test]
    #[ignore]
    fn seek_bar_screenshot() {
        let out_dir = std::path::PathBuf::from(
            std::env::var("NEOWAVES_SHOT_DIR").unwrap_or_else(|_| "/tmp".to_string()),
        );
        std::fs::create_dir_all(&out_dir).ok();

        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        harness.state_mut().test_set_auto_play_list_nav(true);
        let row = wait_for_seekable_row(&mut harness, 0.5, None);
        harness.state_mut().test_select_row_with_autoscroll(row);
        harness.run_steps(6);
        harness.state_mut().test_list_seek_row_frac(row, 0.45);
        harness.run_steps(6);

        // Also assert what the picture is supposed to show, so this cannot
        // quietly become a screenshot of nothing.
        let frac = harness
            .state()
            .test_list_playhead_frac()
            .expect("the sounding row should report a playhead");
        assert!(
            (frac - 0.45).abs() < 0.1,
            "playhead should be drawn near the seek position, got {frac}"
        );

        let image = harness.render().expect("render image");
        let out = out_dir.join("list_seek_bar.png");
        image.save(&out).expect("save screenshot");
        eprintln!("[shot] wrote {}", out.display());
    }

    /// Screenshot of the list scrolled to its end, showing the end-of-list row.
    ///   cargo test --features kittest_render -- --ignored end_of_list_screenshot --nocapture
    #[cfg(feature = "kittest_render")]
    #[test]
    #[ignore]
    fn end_of_list_screenshot() {
        let out_dir = std::path::PathBuf::from(
            std::env::var("NEOWAVES_SHOT_DIR").unwrap_or_else(|_| "/tmp".to_string()),
        );
        std::fs::create_dir_all(&out_dir).ok();

        let mut harness = harness_with_wavs(false);
        wait_for_scan(&mut harness);
        harness.state_mut().test_list_scroll_to_end();
        harness.run_steps(4);

        let image = harness.render().expect("render image");
        let out = out_dir.join("list_end_of_list.png");
        image.save(&out).expect("save screenshot");
        eprintln!("[shot] wrote {}", out.display());

        assert!(
            harness.state().test_list_end_row_fully_visible(),
            "the picture is supposed to show the end-of-list row"
        );
    }

    /// The list ends with a row stating the total. Reaching it is what tells
    /// the user they are at the end -- previously they had to infer it from a
    /// half-drawn row, and a row clipped by the viewport looked exactly like a
    /// row with more below it.
    #[test]
    fn scrolling_to_the_end_shows_the_end_of_list_row() {
        for count in [1usize, 5, 400, 4_000] {
            let mut harness = harness_with_startup(StartupConfig {
                dummy_list_count: Some(count),
                ..StartupConfig::default()
            });
            harness.run_steps(3);
            harness.state_mut().test_list_scroll_to_end();
            harness.run_steps(3);

            assert!(
                harness.state().test_list_end_row_fully_visible(),
                "count={count}: end-of-list row was not fully on screen \
                 (scroll_row={}, last_fully_visible={:?})",
                harness.state().test_list_scroll_row(),
                harness.state().test_list_last_fully_visible_row()
            );
            // ...and the last actual file sits above it, still fully visible.
            assert_eq!(
                harness.state().test_list_last_fully_visible_row(),
                Some(count - 1),
                "count={count}: last file not fully on screen"
            );
        }
    }

    /// A list shorter than the viewport never scrolls, but must still show the
    /// closing row.
    #[test]
    fn a_short_list_still_shows_the_end_of_list_row() {
        let mut harness = harness_with_startup(StartupConfig {
            dummy_list_count: Some(3),
            ..StartupConfig::default()
        });
        harness.run_steps(3);
        assert_eq!(harness.state().test_list_scroll_row(), 0);
        assert!(harness.state().test_list_end_row_fully_visible());
    }

    /// `Delete` is now the editor's Cut, and the list and the effect graph
    /// already use Delete for their own removals. Each is scoped to its own
    /// workspace; this pins that down in both directions, because a leak here
    /// destroys audio (or list rows) the user never targeted.
    #[test]
    fn editor_delete_key_does_not_leak_between_workspaces() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);

        let tab_idx = harness.state().active_tab.expect("active tab");
        assert!(harness.state_mut().test_set_selection_frac(0.4, 0.6));
        harness.run_steps(1);
        let len_before = harness.state().tabs[tab_idx].samples_len;

        // Showing the list, with the editor tab still open in the background.
        harness.state_mut().test_switch_to_list_workspace();
        harness.run_steps(1);
        harness.key_press(Key::Delete);
        harness.run_steps(2);
        assert_eq!(
            harness.state().tabs[tab_idx].samples_len,
            len_before,
            "Delete in the list workspace must not edit a background editor tab"
        );

        // The Recording workspace is the case that actually needs the editor
        // guard: unlike the list and the effect graph, nothing there consumes
        // Delete first, so an unguarded binding would silently cut audio in a
        // tab that is not on screen.
        harness.state_mut().test_open_recording_tab();
        harness.run_steps(1);
        harness.key_press(Key::Delete);
        harness.run_steps(2);
        assert_eq!(
            harness.state().tabs[tab_idx].samples_len,
            len_before,
            "Delete in the recording workspace must not edit a background editor tab"
        );

        // Back in the editor it performs the cut.
        harness.state_mut().test_set_workspace_editor();
        harness.run_steps(1);
        harness.key_press(Key::Delete);
        harness.run_steps(2);
        assert!(
            harness.state().tabs[tab_idx].samples_len < len_before,
            "Delete in the editor workspace should cut the selection"
        );
    }

    /// The Metadata inspector replaces the waveform, so the destructive
    /// selection keys stand down there — Delete especially, which people press
    /// reflexively in a table of fields.
    #[test]
    fn destructive_keys_stand_down_in_the_metadata_view() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);

        let tab_idx = harness.state().active_tab.expect("active tab");
        assert!(harness.state_mut().test_set_selection_frac(0.4, 0.6));
        harness.run_steps(1);
        let len_before = harness.state().tabs[tab_idx].samples_len;

        assert!(harness.state_mut().test_set_metadata_view(false));
        harness.run_steps(1);

        for key in [Key::Delete, Key::T] {
            harness.key_press(key);
            harness.run_steps(2);
        }
        harness.key_press_modifiers(Modifiers::COMMAND, Key::M);
        harness.run_steps(2);

        assert_eq!(
            harness.state().tabs[tab_idx].samples_len,
            len_before,
            "no destructive key may edit audio that the Metadata view is hiding"
        );
    }

    /// The Trim tool used to paint an orange band for `trim_range` on top of
    /// the blue selection. After Auto Trim both hold the same span, so the one
    /// range was drawn twice and read as "a second range you had to set" —
    /// which is exactly the misunderstanding this removal addresses. Assert on
    /// pixels, because only a render can catch the paint coming back.
    #[cfg(feature = "kittest_render")]
    #[test]
    fn no_orange_trim_band_is_painted() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);

        assert!(harness.state_mut().test_set_active_tool(ToolKind::Trim));
        assert!(harness.state_mut().test_set_trim_range_frac(0.20, 0.60));
        harness.run_steps(3);
        assert!(
            harness.state().tabs[harness.state().active_tab.expect("active tab")]
                .trim_range
                .is_some(),
            "the range must still be set, or this test proves nothing"
        );

        let image = harness.render().expect("render image");
        // The retired band's stroke was (255,140,0) at alpha 190; over the dark
        // canvas that lands near (195,110,7). Match that hue specifically --
        // green well under 3/4 of red, almost no blue -- rather than "warm",
        // which also catches the gold (195,166,77) label text in the inspector.
        let is_band_orange = |p: &image::Rgba<u8>| {
            let [r, g, b, _a] = p.0;
            r > 180 && (g as f32) < (r as f32) * 0.72 && b < 60
        };
        let orange = image.pixels().filter(|p| is_band_orange(p)).count();
        assert_eq!(orange, 0, "found {orange} orange trim-band pixels");
    }

    /// Position the pointer on a selection edge at the canvas's *nominal* y —
    /// which `editor_canvas_pos_at_x_offset` puts only a few pixels below the
    /// canvas top, inside the Time Stretch grip's band. Use
    /// `editor_pos_at_selection_edge_body` for the plain resize part of the
    /// same line and `editor_pos_at_selection_stretch_handle` for the grip.
    fn editor_pos_at_selection_boundary(
        harness: &Harness<'static, WavesPreviewer>,
        display_sample: usize,
    ) -> egui::Pos2 {
        let x = harness
            .state()
            .test_editor_display_sample_boundary_x_offset(display_sample)
            .expect("boundary x");
        editor_canvas_pos_at_x_offset(harness, x)
    }

    /// Position the pointer on a selection edge's line well below the Time
    /// Stretch grip, where dragging only moves the range. Measured from the
    /// real canvas rect, because the y that `editor_canvas_pos_at_x_offset`
    /// produces sits inside the grip.
    fn editor_pos_at_selection_edge_body(
        harness: &Harness<'static, WavesPreviewer>,
        display_sample: usize,
    ) -> egui::Pos2 {
        let x = harness
            .state()
            .test_editor_display_sample_boundary_x_offset(display_sample)
            .expect("boundary x");
        let canvas = harness
            .state()
            .test_editor_wave_canvas_rect()
            .expect("wave canvas rect");
        egui::pos2(editor_wave_left(harness) + x, canvas.center().y)
    }

    /// Position the pointer on a selection edge's Time Stretch grip — the tab
    /// at the very top of the canvas, and the only part of the edge that
    /// rewrites audio. The Y is what tells the two gestures apart.
    fn editor_pos_at_selection_stretch_handle(
        harness: &Harness<'static, WavesPreviewer>,
        display_sample: usize,
    ) -> egui::Pos2 {
        let x = harness
            .state()
            .test_editor_display_sample_boundary_x_offset(display_sample)
            .expect("boundary x");
        let canvas = harness
            .state()
            .test_editor_wave_canvas_rect()
            .expect("wave canvas rect");
        let handle_h = WavesPreviewer::test_editor_selection_stretch_handle_height();
        egui::pos2(editor_wave_left(harness) + x, canvas.top() + handle_h * 0.5)
    }

    fn editor_selection(harness: &Harness<'static, WavesPreviewer>) -> (usize, usize) {
        let tab_idx = harness.state().active_tab.expect("active tab");
        let (a, b) = harness.state().tabs[tab_idx].selection.expect("selection");
        if a <= b {
            (a, b)
        } else {
            (b, a)
        }
    }

    /// The handles are tool-independent and start a destructive,
    /// pitch-preserving Time Stretch only on pointer release.
    #[test]
    fn selection_handle_drag_applies_time_stretch_from_gain_tool_on_release() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);

        assert!(harness.state_mut().test_set_active_tool(ToolKind::Gain));
        assert!(harness.state_mut().test_set_selection_frac(0.30, 0.50));
        harness.run_steps(2);
        let tab_idx = harness.state().active_tab.expect("active tab");
        let selection_before = editor_selection(&harness);
        let len_before = harness.state().tabs[tab_idx].samples_len;
        let inspector_rate_before = harness.state().tabs[tab_idx].tool_state.stretch_rate;
        let undo_before = harness.state().tabs[tab_idx].undo_stack.len();
        let from = editor_pos_at_selection_stretch_handle(&harness, selection_before.1);
        let to = egui::pos2(from.x + 90.0, from.y);

        harness.hover_at(from);
        harness.event(egui::Event::PointerButton {
            pos: from,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: Modifiers::NONE,
        });
        harness.run_steps(1);
        harness.event(egui::Event::PointerMoved(to));
        harness.run_steps(2);

        let held = harness.state().tabs[tab_idx]
            .selection_stretch_gesture
            .expect("selection stretch gesture while held");
        assert_eq!(format!("{:?}", held.edge), "End");
        assert!(held.target_len > selection_before.1 - selection_before.0);
        assert_eq!(
            editor_selection(&harness),
            selection_before,
            "dragging may update only the ghost, not the committed selection"
        );
        assert_eq!(harness.state().tabs[tab_idx].samples_len, len_before);
        assert!(!harness.state().test_editor_apply_active());

        harness.event(egui::Event::PointerButton {
            pos: to,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        });
        harness.run_steps(1);
        wait_for_editor_apply(&mut harness);
        harness.run_steps(2);

        let selection_after = editor_selection(&harness);
        assert_eq!(selection_after.0, selection_before.0);
        assert!(selection_after.1 > selection_before.1);
        assert!(harness.state().tabs[tab_idx].samples_len > len_before);
        assert_eq!(harness.state().test_active_tool(), Some(ToolKind::Gain));
        assert_eq!(
            harness.state().tabs[tab_idx].tool_state.stretch_rate,
            inspector_rate_before,
            "the direct gesture must not change the Time Stretch inspector rate"
        );
        assert_eq!(
            harness.state().tabs[tab_idx].undo_stack.len(),
            undo_before + 1,
            "one handle release must create exactly one Undo step"
        );
    }

    /// A start-handle apply ripples the whole buffer without losing prefix or
    /// suffix audio, while the output range's fixed end stays at the same x.
    #[test]
    fn selection_start_handle_preserves_audio_and_fixed_edge_viewport() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);

        assert!(harness
            .state_mut()
            .test_set_active_tool(ToolKind::Normalize));
        assert!(harness.state_mut().test_set_tab_samples_per_px(40.0));
        assert!(harness.state_mut().test_set_tab_view_offset(40_000));
        assert!(harness.state_mut().test_set_selection_frac(0.32, 0.48));
        harness.run_steps(2);
        let tab_idx = harness.state().active_tab.expect("active tab");
        let (start_before, end_before) = editor_selection(&harness);
        let len_before = harness.state().tabs[tab_idx].samples_len;
        let prefix_before = harness.state().tabs[tab_idx].ch_samples[0][..128].to_vec();
        let suffix_before =
            harness.state().tabs[tab_idx].ch_samples[0][len_before - 128..].to_vec();
        let fixed_x_before = harness
            .state()
            .test_editor_display_sample_boundary_x_offset(end_before)
            .expect("fixed end x before");

        let from = editor_pos_at_selection_stretch_handle(&harness, start_before);
        editor_pointer_drag(&mut harness, from, egui::pos2(from.x + 50.0, from.y));
        wait_for_editor_apply(&mut harness);
        harness.run_steps(3);

        let (start_after, end_after) = editor_selection(&harness);
        let len_after = harness.state().tabs[tab_idx].samples_len;
        assert_eq!(start_after, start_before);
        assert!(end_after < end_before, "start drag right should shorten");
        assert!(len_after < len_before);
        assert_eq!(
            &harness.state().tabs[tab_idx].ch_samples[0][..128],
            prefix_before.as_slice(),
            "audio before the replacement must be preserved"
        );
        assert_eq!(
            &harness.state().tabs[tab_idx].ch_samples[0][len_after - 128..],
            suffix_before.as_slice(),
            "audio after the replacement must ripple and remain preserved"
        );
        let fixed_x_after = harness
            .state()
            .test_editor_display_sample_boundary_x_offset(end_after)
            .expect("fixed end x after");
        assert_eq!(
            harness.state().test_active_tool(),
            Some(ToolKind::Normalize),
            "the selected inspector tool must not change"
        );
        assert!(
            (fixed_x_after - fixed_x_before).abs() <= 1.5,
            "fixed end moved on screen: {fixed_x_before} -> {fixed_x_after}"
        );

        harness.key_press_modifiers(Modifiers::COMMAND, Key::Z);
        harness.run_steps(3);
        assert_eq!(harness.state().tabs[tab_idx].samples_len, len_before);
    }

    /// A grab with no movement reads to egui as a click, and the click handler
    /// clears the selection and seeks. `suppress_seek` has to survive the
    /// release frame or fine-tuning would destroy the thing being tuned.
    #[test]
    fn selection_edge_grab_without_movement_keeps_the_selection() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);

        assert!(harness.state_mut().test_set_selection_frac(0.30, 0.60));
        harness.run_steps(2);
        let before = editor_selection(&harness);

        let at = editor_pos_at_selection_edge_body(&harness, before.1);
        editor_pointer_drag(&mut harness, at, at);

        assert_eq!(
            editor_selection(&harness),
            before,
            "a click on a handle must not clear the selection"
        );
    }

    /// Same for the grip at the top: a press that never moves must leave both
    /// the selection and the audio exactly as they were.
    #[test]
    fn selection_stretch_grip_grab_without_movement_changes_nothing() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);

        assert!(harness.state_mut().test_set_selection_frac(0.30, 0.60));
        harness.run_steps(2);
        let tab_idx = harness.state().active_tab.expect("active tab");
        let before = editor_selection(&harness);
        let len_before = harness.state().tabs[tab_idx].samples_len;
        let undo_before = harness.state().tabs[tab_idx].undo_stack.len();

        let at = editor_pos_at_selection_stretch_handle(&harness, before.1);
        editor_pointer_drag(&mut harness, at, at);

        assert_eq!(
            editor_selection(&harness),
            before,
            "a click on the grip must not clear the selection"
        );
        assert!(!harness.state().test_editor_apply_active());
        assert_eq!(harness.state().tabs[tab_idx].samples_len, len_before);
        assert_eq!(
            harness.state().tabs[tab_idx].undo_stack.len(),
            undo_before,
            "a grip click with no movement is not an edit"
        );
    }

    /// The edge line below the grip only moves the range. No resample, no
    /// worker, no undo step — the audio is untouched.
    #[test]
    fn selection_edge_body_drag_resizes_without_stretching() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);

        assert!(harness.state_mut().test_set_active_tool(ToolKind::Gain));
        assert!(harness.state_mut().test_set_selection_frac(0.30, 0.50));
        harness.run_steps(2);
        let tab_idx = harness.state().active_tab.expect("active tab");
        let before = editor_selection(&harness);
        let len_before = harness.state().tabs[tab_idx].samples_len;
        let undo_before = harness.state().tabs[tab_idx].undo_stack.len();

        let from = editor_pos_at_selection_edge_body(&harness, before.1);
        let to = egui::pos2(from.x + 90.0, from.y);
        editor_pointer_drag(&mut harness, from, to);

        let after = editor_selection(&harness);
        assert_eq!(after.0, before.0, "the opposite edge stays put");
        assert!(
            after.1 > before.1,
            "dragging the end edge outward lengthens the range: {before:?} -> {after:?}"
        );
        assert!(
            harness.state().tabs[tab_idx]
                .selection_stretch_gesture
                .is_none(),
            "the body of the edge line must not arm a stretch"
        );
        assert!(!harness.state().test_editor_apply_active());
        assert_eq!(
            harness.state().tabs[tab_idx].samples_len,
            len_before,
            "a range resize must not change the buffer length"
        );
        assert_eq!(
            harness.state().tabs[tab_idx].undo_stack.len(),
            undo_before,
            "a range resize is not a destructive edit"
        );
    }

    /// Dragging an edge past the other one flips the range, exactly like
    /// drawing a fresh selection does.
    #[test]
    fn selection_edge_body_drag_past_the_other_edge_flips_the_range() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);

        assert!(harness.state_mut().test_set_selection_frac(0.40, 0.50));
        harness.run_steps(2);
        let tab_idx = harness.state().active_tab.expect("active tab");
        let before = editor_selection(&harness);
        let len_before = harness.state().tabs[tab_idx].samples_len;

        let from = editor_pos_at_selection_edge_body(&harness, before.1);
        let to = egui::pos2(from.x - 160.0, from.y);
        editor_pointer_drag(&mut harness, from, to);

        let after = editor_selection(&harness);
        assert!(
            after.1 <= before.0,
            "the dragged edge crossed the anchor: {before:?} -> {after:?}"
        );
        assert_eq!(
            after.1, before.0,
            "the edge that stayed put becomes the new end"
        );
        assert_eq!(harness.state().tabs[tab_idx].samples_len, len_before);
    }

    /// A loop edge normally sits exactly on the selection edge, so the two
    /// grips overlap. They are split by height: the tab at the top stretches.
    #[test]
    fn selection_stretch_grip_wins_over_the_loop_marker_in_loop_edit() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);

        assert!(harness.state_mut().test_set_active_tool(ToolKind::LoopEdit));
        assert!(harness.state_mut().test_set_selection_frac(0.30, 0.50));
        assert!(harness.state_mut().test_set_loop_region_frac(0.30, 0.50));
        harness.run_steps(2);
        let tab_idx = harness.state().active_tab.expect("active tab");
        let selection_before = editor_selection(&harness);
        let loop_before = harness.state().test_loop_region().expect("loop region");
        assert_eq!(
            loop_before, selection_before,
            "the two ranges must coincide"
        );

        let from = editor_pos_at_selection_stretch_handle(&harness, selection_before.1);
        let to = egui::pos2(from.x + 90.0, from.y);
        harness.hover_at(from);
        harness.event(egui::Event::PointerButton {
            pos: from,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: Modifiers::NONE,
        });
        harness.run_steps(1);
        harness.event(egui::Event::PointerMoved(to));
        harness.run_steps(2);

        let held = harness.state().tabs[tab_idx]
            .selection_stretch_gesture
            .expect("the grip arms the stretch even in Loop Edit");
        assert!(held.target_len > selection_before.1 - selection_before.0);
        assert!(
            harness.state().tabs[tab_idx].dragging_marker.is_none(),
            "the loop marker must not also arm under the same pointer"
        );
        assert_eq!(
            harness.state().test_loop_region(),
            Some(loop_before),
            "the loop range stays where it was while the grip is held"
        );

        harness.event(egui::Event::PointerButton {
            pos: to,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        });
        harness.run_steps(1);
        wait_for_editor_apply(&mut harness);
        harness.run_steps(2);
    }

    /// Both gestures arm from `pointer_down` rather than the press, so a resize
    /// that is already under way must keep the pointer even when it sweeps past
    /// a loop marker — otherwise the range freezes mid-drag and the loop moves
    /// instead.
    #[test]
    fn a_loop_marker_does_not_hijack_an_edge_resize_in_progress() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);

        assert!(harness.state_mut().test_set_active_tool(ToolKind::LoopEdit));
        assert!(harness.state_mut().test_set_selection_frac(0.40, 0.70));
        assert!(harness.state_mut().test_set_loop_region_frac(0.10, 0.20));
        harness.run_steps(2);
        let tab_idx = harness.state().active_tab.expect("active tab");
        let selection_before = editor_selection(&harness);
        let loop_before = harness.state().test_loop_region().expect("loop region");

        // Start on the selection's start edge and sweep left, across the loop
        // markers, in steps — the hijack only shows up on an intermediate frame.
        let from = editor_pos_at_selection_edge_body(&harness, selection_before.0);
        let loop_end_x = editor_pos_at_selection_edge_body(&harness, loop_before.1).x;
        let past_loop = egui::pos2(loop_end_x - 20.0, from.y);
        harness.hover_at(from);
        harness.event(egui::Event::PointerButton {
            pos: from,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: Modifiers::NONE,
        });
        harness.run_steps(1);
        let steps = 8;
        for step in 1..=steps {
            let t = step as f32 / steps as f32;
            harness.event(egui::Event::PointerMoved(egui::pos2(
                from.x + (past_loop.x - from.x) * t,
                from.y,
            )));
            harness.run_steps(1);
        }
        harness.event(egui::Event::PointerButton {
            pos: past_loop,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        });
        harness.run_steps(2);

        assert_eq!(
            harness.state().test_loop_region(),
            Some(loop_before),
            "the loop range must not move while an edge resize owns the pointer"
        );
        let after = editor_selection(&harness);
        assert_eq!(after.1, selection_before.1, "the opposite edge stays put");
        assert!(
            after.0 < loop_before.1,
            "the dragged edge followed the pointer past the loop end: \
             {selection_before:?} -> {after:?}, loop {loop_before:?}"
        );
        assert!(harness.state().tabs[tab_idx].dragging_marker.is_none());
    }

    /// ...and the line below the grip still belongs to the loop marker, which
    /// is what the Loop Edit tool is for.
    #[test]
    fn loop_marker_drag_still_owns_the_edge_body_in_loop_edit() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);

        assert!(harness.state_mut().test_set_active_tool(ToolKind::LoopEdit));
        assert!(harness.state_mut().test_set_selection_frac(0.30, 0.50));
        assert!(harness.state_mut().test_set_loop_region_frac(0.30, 0.50));
        harness.run_steps(2);
        let tab_idx = harness.state().active_tab.expect("active tab");
        let selection_before = editor_selection(&harness);
        let loop_before = harness.state().test_loop_region().expect("loop region");
        let len_before = harness.state().tabs[tab_idx].samples_len;

        let from = editor_pos_at_selection_edge_body(&harness, selection_before.1);
        let to = egui::pos2(from.x + 90.0, from.y);
        harness.hover_at(from);
        harness.event(egui::Event::PointerButton {
            pos: from,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: Modifiers::NONE,
        });
        harness.run_steps(1);
        harness.event(egui::Event::PointerMoved(to));
        harness.run_steps(2);

        assert!(
            harness.state().tabs[tab_idx].dragging_marker.is_some(),
            "the body of the line is the loop marker's in Loop Edit"
        );
        assert!(
            harness.state().tabs[tab_idx]
                .selection_stretch_gesture
                .is_none(),
            "no stretch below the grip"
        );
        let loop_held = harness.state().test_loop_region().expect("loop region");
        assert!(
            loop_held.1 > loop_before.1,
            "the loop end followed the pointer: {loop_before:?} -> {loop_held:?}"
        );
        assert_eq!(
            editor_selection(&harness),
            selection_before,
            "the loop marker drag leaves the selection alone"
        );

        harness.event(egui::Event::PointerButton {
            pos: to,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        });
        harness.run_steps(2);
        assert!(!harness.state().test_editor_apply_active());
        assert_eq!(harness.state().tabs[tab_idx].samples_len, len_before);
    }

    /// Clicking well inside the selection is still a seek that clears it —
    /// the grab must not swallow the whole range.
    #[test]
    fn clicking_inside_the_selection_still_clears_it() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);

        assert!(harness.state_mut().test_set_selection_frac(0.20, 0.80));
        harness.run_steps(2);
        let (start, end) = editor_selection(&harness);

        let mid = editor_pos_at_selection_boundary(&harness, (start + end) / 2);
        editor_pointer_drag(&mut harness, mid, mid);

        let tab_idx = harness.state().active_tab.expect("active tab");
        assert!(
            harness.state().tabs[tab_idx].selection.is_none(),
            "a click away from the handles should still seek and clear"
        );
    }

    /// The Time Stretch grip is Waveform-only: a waveform overlay does not put
    /// one on an analysis view. Dragging the edge there is the plain range
    /// resize, which touches no audio.
    #[test]
    fn selection_handles_do_not_apply_in_non_waveform_views() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);

        assert!(harness.state_mut().test_set_waveform_overlay(true));
        let tab_idx = harness.state().active_tab.expect("active tab");
        for mode in [
            neowaves::ViewMode::Spectrogram,
            neowaves::ViewMode::Log,
            neowaves::ViewMode::Mel,
            neowaves::ViewMode::Tempogram,
            neowaves::ViewMode::Chromagram,
            neowaves::ViewMode::World,
        ] {
            assert!(harness.state_mut().test_set_view_mode(mode));
            assert!(harness.state_mut().test_set_selection_frac(0.30, 0.50));
            harness.run_steps(2);
            let (_, end_before) = editor_selection(&harness);
            let len_before = harness.state().tabs[tab_idx].samples_len;
            let from = editor_pos_at_selection_boundary(&harness, end_before);
            editor_pointer_drag(&mut harness, from, egui::pos2(from.x + 70.0, from.y));
            assert!(
                harness.state().tabs[tab_idx]
                    .selection_stretch_gesture
                    .is_none(),
                "{mode:?} must not arm a waveform handle"
            );
            assert!(!harness.state().test_editor_apply_active());
            assert_eq!(harness.state().tabs[tab_idx].samples_len, len_before);
        }
    }

    /// Escape while the grip is held drops the gesture, and the rest of that
    /// held press stays swallowed so it cannot fall through into a fresh
    /// range selection.
    #[test]
    fn selection_handle_escape_cancels_without_applying() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);

        assert!(harness.state_mut().test_set_selection_frac(0.30, 0.50));
        harness.run_steps(2);
        let tab_idx = harness.state().active_tab.expect("active tab");
        let selection_before = editor_selection(&harness);
        let len_before = harness.state().tabs[tab_idx].samples_len;
        let from = editor_pos_at_selection_stretch_handle(&harness, selection_before.1);
        let to = egui::pos2(from.x + 70.0, from.y);
        harness.hover_at(from);
        harness.event(egui::Event::PointerButton {
            pos: from,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: Modifiers::NONE,
        });
        harness.run_steps(1);
        harness.event(egui::Event::PointerMoved(to));
        harness.run_steps(2);
        assert!(harness.state().tabs[tab_idx]
            .selection_stretch_gesture
            .is_some());

        harness.key_press(Key::Escape);
        harness.run_steps(1);
        assert!(harness.state().tabs[tab_idx]
            .selection_stretch_gesture
            .is_none());
        harness.event(egui::Event::PointerButton {
            pos: to,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        });
        harness.run_steps(2);
        assert!(!harness.state().test_editor_apply_active());
        assert_eq!(harness.state().tabs[tab_idx].samples_len, len_before);
        assert_eq!(editor_selection(&harness), selection_before);
    }

    #[test]
    fn selection_handle_rejects_a_busy_apply_without_queueing() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness.state_mut().test_set_selection_frac(0.30, 0.50));
        harness.run_steps(2);
        let tab_idx = harness.state().active_tab.expect("active tab");
        let len_before = harness.state().tabs[tab_idx].samples_len;
        let (_, end) = editor_selection(&harness);
        let from = editor_pos_at_selection_stretch_handle(&harness, end);
        let to = egui::pos2(from.x + 70.0, from.y);
        harness.hover_at(from);
        harness.event(egui::Event::PointerButton {
            pos: from,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: Modifiers::NONE,
        });
        harness.run_steps(1);
        harness.event(egui::Event::PointerMoved(to));
        harness.run_steps(1);
        assert!(harness.state_mut().test_set_mock_editor_apply_busy());
        harness.event(egui::Event::PointerButton {
            pos: to,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        });
        harness.run_steps(2);
        assert!(harness
            .state()
            .test_toast_messages()
            .iter()
            .any(|message| message.contains("Another apply")));
        harness.state_mut().test_clear_mock_editor_apply_busy();
        harness.run_steps(2);
        assert!(!harness.state().test_editor_apply_active());
        assert_eq!(harness.state().tabs[tab_idx].samples_len, len_before);
    }

    #[test]
    fn selection_handle_rejects_loading_audio_without_queueing() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness.state_mut().test_set_selection_frac(0.30, 0.50));
        harness.run_steps(2);
        let tab_idx = harness.state().active_tab.expect("active tab");
        let len_before = harness.state().tabs[tab_idx].samples_len;
        let (_, end) = editor_selection(&harness);
        let from = editor_pos_at_selection_stretch_handle(&harness, end);
        let to = egui::pos2(from.x + 70.0, from.y);
        harness.hover_at(from);
        harness.event(egui::Event::PointerButton {
            pos: from,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: Modifiers::NONE,
        });
        harness.run_steps(1);
        harness.event(egui::Event::PointerMoved(to));
        harness.run_steps(1);
        assert!(harness.state_mut().test_set_tab_loading(true));
        harness.event(egui::Event::PointerButton {
            pos: to,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        });
        harness.run_steps(2);
        assert!(harness
            .state()
            .test_toast_messages()
            .iter()
            .any(|message| message.contains("finishes loading")));
        assert!(harness.state_mut().test_set_tab_loading(false));
        harness.run_steps(2);
        assert!(!harness.state().test_editor_apply_active());
        assert_eq!(harness.state().tabs[tab_idx].samples_len, len_before);
    }

    #[test]
    fn selection_handle_rejects_decode_failure_without_queueing() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness.state_mut().test_set_selection_frac(0.30, 0.50));
        harness.run_steps(2);
        let tab_idx = harness.state().active_tab.expect("active tab");
        let len_before = harness.state().tabs[tab_idx].samples_len;
        let (_, end) = editor_selection(&harness);
        let from = editor_pos_at_selection_stretch_handle(&harness, end);
        let to = egui::pos2(from.x + 70.0, from.y);
        harness.hover_at(from);
        harness.event(egui::Event::PointerButton {
            pos: from,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: Modifiers::NONE,
        });
        harness.run_steps(1);
        harness.event(egui::Event::PointerMoved(to));
        harness.run_steps(1);
        assert!(harness
            .state_mut()
            .test_set_active_decode_error(Some("fixture decode failure")));
        harness.event(egui::Event::PointerButton {
            pos: to,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        });
        harness.run_steps(2);
        assert!(harness
            .state()
            .test_toast_messages()
            .iter()
            .any(|message| message.contains("decoding failed")));
        assert!(harness.state_mut().test_set_active_decode_error(None));
        harness.run_steps(2);
        assert!(!harness.state().test_editor_apply_active());
        assert_eq!(harness.state().tabs[tab_idx].samples_len, len_before);
    }

    #[test]
    fn selection_handle_release_outside_canvas_commits_the_clamped_target() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness.state_mut().test_set_selection_frac(0.30, 0.40));
        harness.run_steps(2);
        let tab_idx = harness.state().active_tab.expect("active tab");
        let selection_before = editor_selection(&harness);
        let source_len = selection_before.1 - selection_before.0;
        let from = editor_pos_at_selection_stretch_handle(&harness, selection_before.1);
        let outside = egui::pos2(from.x + 2_000.0, from.y);
        harness.hover_at(from);
        harness.event(egui::Event::PointerButton {
            pos: from,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: Modifiers::NONE,
        });
        harness.run_steps(1);
        harness.event(egui::Event::PointerMoved(outside));
        harness.run_steps(2);
        assert_eq!(
            harness.state().tabs[tab_idx]
                .selection_stretch_gesture
                .expect("held outside")
                .target_len,
            source_len * 4,
            "outside drag should clamp at the 0.25x limit"
        );
        harness.event(egui::Event::PointerButton {
            pos: outside,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        });
        harness.run_steps(1);
        wait_for_editor_apply(&mut harness);
        harness.run_steps(2);
        let selection_after = editor_selection(&harness);
        assert!(selection_after.1 - selection_after.0 > source_len * 3);
    }

    #[test]
    fn selection_handle_view_switch_cancels_without_applying() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness.state_mut().test_set_selection_frac(0.30, 0.50));
        harness.run_steps(2);
        let tab_idx = harness.state().active_tab.expect("active tab");
        let selection_before = editor_selection(&harness);
        let len_before = harness.state().tabs[tab_idx].samples_len;
        let from = editor_pos_at_selection_stretch_handle(&harness, selection_before.1);
        let to = egui::pos2(from.x + 70.0, from.y);
        harness.hover_at(from);
        harness.event(egui::Event::PointerButton {
            pos: from,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: Modifiers::NONE,
        });
        harness.run_steps(1);
        harness.event(egui::Event::PointerMoved(to));
        harness.run_steps(1);
        assert!(harness
            .state_mut()
            .test_set_view_mode(neowaves::ViewMode::Spectrogram));
        harness.run_steps(2);
        assert!(harness.state().tabs[tab_idx]
            .selection_stretch_gesture
            .is_none());
        harness.event(egui::Event::PointerButton {
            pos: to,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        });
        harness.run_steps(2);
        assert!(!harness.state().test_editor_apply_active());
        assert_eq!(harness.state().tabs[tab_idx].samples_len, len_before);
        assert_eq!(editor_selection(&harness), selection_before);
    }

    /// Visual evidence set for Waveform-only handles, held ghost, and apply.
    ///   cargo test --features kittest_render -- --ignored selection_time_stretch_handle_screenshots --nocapture
    #[cfg(feature = "kittest_render")]
    #[test]
    #[ignore]
    fn selection_time_stretch_handle_screenshots() {
        let out_dir = std::path::PathBuf::from(
            std::env::var("NEOWAVES_SHOT_DIR").unwrap_or_else(|_| "/tmp".to_string()),
        );
        std::fs::create_dir_all(&out_dir).ok();

        let mut harness = harness_with_dynamic_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness.state_mut().test_set_active_tool(ToolKind::Gain));
        assert!(harness.state_mut().test_set_selection_frac(0.25, 0.55));
        harness.run_steps(3);

        let image = harness.render().expect("render image");
        image
            .save(out_dir.join("01_waveform_initial.png"))
            .expect("save screenshot");

        assert!(harness.state_mut().test_set_waveform_overlay(true));
        assert!(harness
            .state_mut()
            .test_set_view_mode(neowaves::ViewMode::Spectrogram));
        harness.run_steps(8);
        harness
            .render()
            .expect("render non-waveform")
            .save(out_dir.join("02_non_waveform.png"))
            .expect("save non-waveform screenshot");

        assert!(harness
            .state_mut()
            .test_set_view_mode(neowaves::ViewMode::Waveform));
        harness.run_steps(3);
        let (_, selection_end) = editor_selection(&harness);
        let from = editor_pos_at_selection_stretch_handle(&harness, selection_end);
        let to = egui::pos2(from.x + 110.0, from.y);
        harness.hover_at(from);
        harness.event(egui::Event::PointerButton {
            pos: from,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: Modifiers::NONE,
        });
        harness.run_steps(1);
        harness.event(egui::Event::PointerMoved(to));
        harness.run_steps(3);
        harness
            .render()
            .expect("render held ghost")
            .save(out_dir.join("03_dragging_ghost.png"))
            .expect("save held-ghost screenshot");

        harness.event(egui::Event::PointerButton {
            pos: to,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        });
        harness.run_steps(1);
        wait_for_editor_apply(&mut harness);
        harness.run_steps(3);
        harness
            .render()
            .expect("render applied waveform")
            .save(out_dir.join("04_applied.png"))
            .expect("save applied screenshot");

        for name in [
            "01_waveform_initial.png",
            "02_non_waveform.png",
            "03_dragging_ghost.png",
            "04_applied.png",
        ] {
            eprintln!("[shot] wrote {}", out_dir.join(name).display());
        }
    }

    /// Position the pointer on a loop edge's line, below the Time Stretch grip
    /// and below the loop's own handle — the part of the line that answers a
    /// loop drag.
    fn editor_pos_at_loop_edge(
        harness: &Harness<'static, WavesPreviewer>,
        display_sample: usize,
    ) -> egui::Pos2 {
        let x = harness
            .state()
            .test_editor_display_sample_boundary_x_offset(display_sample)
            .expect("boundary x");
        let canvas = harness
            .state()
            .test_editor_wave_canvas_rect()
            .expect("wave canvas rect");
        egui::pos2(editor_wave_left(harness) + x, canvas.center().y)
    }

    #[test]
    fn loop_edge_click_seeks_and_leaves_the_loop_alone() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness.state_mut().test_set_active_tool(ToolKind::LoopEdit));
        assert!(harness.state_mut().test_set_loop_region_frac(0.30, 0.70));
        harness.run_steps(2);

        let tab_idx = harness.state().active_tab.expect("active tab");
        let before = harness.state().test_loop_region().expect("loop region");
        let undo_before = harness.state().tabs[tab_idx].undo_stack.len();

        // Press and release on the loop's start edge without moving. This used
        // to drag the edge onto the clicked pixel on the press frame.
        let pos = editor_pos_at_loop_edge(&harness, before.0);
        editor_primary_click_at_pos(&mut harness, pos);

        assert_eq!(
            harness.state().test_loop_region(),
            Some(before),
            "a click on a loop edge must not move it"
        );
        assert_eq!(
            harness.state().tabs[tab_idx].undo_stack.len(),
            undo_before,
            "a click that changes nothing must not push an undo step"
        );
        let playhead = harness
            .state()
            .test_playhead_display_now()
            .expect("playhead after clicking a loop edge");
        assert!(
            playhead.abs_diff(before.0) < harness.state().tabs[tab_idx].samples_len / 20,
            "the click should have seeked to the loop edge: {playhead} vs {}",
            before.0
        );
    }

    #[test]
    fn loop_edge_drag_moves_the_loop() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness.state_mut().test_set_active_tool(ToolKind::LoopEdit));
        assert!(harness.state_mut().test_clear_markers());
        assert!(harness.state_mut().test_set_zero_cross_snap(false));
        assert!(harness.state_mut().test_set_loop_region_frac(0.30, 0.70));
        harness.run_steps(2);

        let before = harness.state().test_loop_region().expect("loop region");
        let from = editor_pos_at_loop_edge(&harness, before.1);
        let to = egui::pos2(from.x - 80.0, from.y);
        editor_pointer_drag(&mut harness, from, to);

        let after = harness.state().test_loop_region().expect("loop region");
        assert_eq!(after.0, before.0, "the opposite edge stays put");
        assert!(
            after.1 < before.1,
            "dragging the end edge inward shortens the loop: {before:?} -> {after:?}"
        );
    }

    #[test]
    fn loop_edge_drag_lands_exactly_on_a_marker() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness.state_mut().test_set_active_tool(ToolKind::LoopEdit));
        assert!(harness.state_mut().test_clear_markers());
        assert!(harness.state_mut().test_add_marker_frac(0.50));
        assert!(harness.state_mut().test_set_loop_region_frac(0.20, 0.80));
        harness.run_steps(2);

        let marker = harness.state().test_marker_samples()[0];
        let before = harness.state().test_loop_region().expect("loop region");

        // Aim a few pixels short of the marker, inside the magnet's reach.
        let from = editor_pos_at_loop_edge(&harness, before.0);
        let marker_x = harness
            .state()
            .test_editor_display_sample_boundary_x_offset(marker)
            .expect("marker x");
        let to = egui::pos2(editor_wave_left(&harness) + marker_x - 4.0, from.y);
        editor_pointer_drag(&mut harness, from, to);

        let after = harness.state().test_loop_region().expect("loop region");
        assert_eq!(
            after.0, marker,
            "a loop edge dropped within the magnet takes the marker's own \
             sample index, not the one the pixel rounds to"
        );
    }

    #[test]
    fn a_loop_scrolled_out_of_view_has_no_handle_at_the_canvas_edge() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness.state_mut().test_set_active_tool(ToolKind::LoopEdit));
        assert!(harness.state_mut().test_clear_markers());
        assert!(harness.state_mut().test_set_zero_cross_snap(false));
        assert!(harness.state_mut().test_set_loop_region_frac(0.02, 0.06));
        harness.run_steps(2);
        let before = harness.state().test_loop_region().expect("loop region");

        // Zoom in on the far end, so both loop edges are off the left of the
        // canvas. `sample_boundary_x` reports them as sitting exactly on the
        // canvas border, which used to make the border grabbable.
        for _ in 0..14 {
            editor_zoom_in_at_frac(&mut harness, 0.90);
        }
        harness.run_steps(2);

        let canvas = harness
            .state()
            .test_editor_wave_canvas_rect()
            .expect("wave canvas rect");
        let from = egui::pos2(editor_wave_left(&harness) + 1.0, canvas.center().y);
        let to = egui::pos2(from.x + 70.0, from.y);
        editor_pointer_drag(&mut harness, from, to);

        assert_eq!(
            harness.state().test_loop_region(),
            Some(before),
            "the canvas border is not a loop handle"
        );
    }

    #[test]
    fn the_end_of_a_narrow_loop_can_still_be_grabbed() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness.state_mut().test_set_active_tool(ToolKind::LoopEdit));
        assert!(harness.state_mut().test_clear_markers());
        assert!(harness.state_mut().test_set_zero_cross_snap(false));
        // Short enough that both edges are inside one grab radius of each
        // other, which is where the old `if / else if` always chose the start.
        assert!(harness.state_mut().test_set_loop_region_frac(0.50, 0.502));
        harness.run_steps(2);

        let before = harness.state().test_loop_region().expect("loop region");
        let from = editor_pos_at_loop_edge(&harness, before.1);
        let to = egui::pos2(from.x + 120.0, from.y);
        editor_pointer_drag(&mut harness, from, to);

        let after = harness.state().test_loop_region().expect("loop region");
        assert_eq!(
            after.0, before.0,
            "the start must not have been the one that moved"
        );
        assert!(
            after.1 > before.1,
            "the end edge should have taken the press: {before:?} -> {after:?}"
        );
    }

    #[test]
    fn arrow_keys_stop_on_a_loop_point() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness.state_mut().test_clear_markers());
        assert!(harness.state_mut().test_set_loop_region_frac(0.30, 0.60));
        harness.run_steps(2);
        let (loop_start, _) = harness.state().test_loop_region().expect("loop region");

        harness.key_press(Key::Home);
        harness.run_steps(2);

        // Step right with the grid. Somewhere on the way the step has to cross
        // the loop start, and when it does the playhead must land exactly on
        // it rather than skip past to the next grid multiple.
        let mut landed = false;
        for _ in 0..40 {
            harness.key_press(Key::ArrowRight);
            harness.run_steps(2);
            let playhead = harness
                .state()
                .test_playhead_display_now()
                .expect("playhead while stepping");
            if playhead == loop_start {
                landed = true;
                break;
            }
            if playhead > loop_start {
                break;
            }
        }
        assert!(
            landed,
            "stepping right past the loop start should have stopped on it ({loop_start})"
        );

        // And the next press has to move on, not sit on the same landmark.
        harness.key_press(Key::ArrowRight);
        harness.run_steps(2);
        assert!(
            harness.state().test_playhead_display_now().unwrap() > loop_start,
            "the playhead must continue past a landmark it already stopped on"
        );
    }

    fn double_click_at(harness: &mut Harness<'static, WavesPreviewer>, pos: egui::Pos2) {
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

    #[test]
    fn the_monitor_volume_starts_at_unity() {
        let harness = harness_with_editor_fixture();
        assert_eq!(
            harness.state().test_volume_db(),
            0.0,
            "a fresh app should monitor at unity, not 12 dB down"
        );
    }

    #[test]
    fn double_clicking_the_volume_control_returns_it_to_unity() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        harness.run_steps(2);
        harness.state_mut().test_set_volume_db(-31.0);
        harness.run_steps(2);
        assert_eq!(harness.state().test_volume_db(), -31.0);

        let rect = harness
            .state()
            .test_topbar_volume_rect()
            .expect("volume control rect");
        // Deliberately off the knob: the whole control resets, and the first
        // click of the pair would otherwise set the volume from the pointer
        // position and hide a reset that never happened.
        double_click_at(&mut harness, egui::pos2(rect.left() + 6.0, rect.center().y));

        assert_eq!(
            harness.state().test_volume_db(),
            0.0,
            "double click should put the monitor back at unity"
        );
        assert!(
            (harness.state().test_audio_output_volume_linear() - 1.0).abs() < 1.0e-4,
            "the reset has to reach the engine, not just the readout"
        );
    }

    #[test]
    fn the_volume_slider_can_actually_reach_its_own_top_end() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        harness.run_steps(2);

        harness.state_mut().test_set_volume_db(0.0);
        harness.run_steps(2);
        let unity = harness.state().test_audio_output_volume_linear();
        assert!(
            (unity - 1.0).abs() < 1.0e-3,
            "0 dB should be unity, got {unity}"
        );

        // The slider advertises +6 dB; the engine used to clamp the linear gain
        // at 1.0, so its whole upper half did nothing.
        harness.state_mut().test_set_volume_db(6.0);
        harness.run_steps(2);
        let boosted = harness.state().test_audio_output_volume_linear();
        assert!(
            boosted > unity * 1.9,
            "+6 dB should roughly double the monitor gain, got {boosted}"
        );
    }

    #[test]
    fn pasting_file_paths_as_text_imports_them_into_the_list() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        harness.run_steps(2);
        let before = harness.state().test_visible_list_paths().len();

        // Files that exist but are not in the open folder, plus two paths that
        // should be turned away: one non-audio, one that is not there at all.
        let extra = make_temp_dir("paste_import");
        let sr = 48_000;
        let chans = synth_stereo(sr, 0.25);
        let mut lines = Vec::new();
        for name in ["pasted_one.wav", "pasted_two.wav"] {
            let path = extra.join(name);
            neowaves::wave::export_channels_audio(&chans, sr, &path)
                .unwrap_or_else(|e| panic!("export {name} failed: {e}"));
            lines.push(format!("file://{}", path.display()));
        }
        let notes = extra.join("notes.txt");
        std::fs::write(&notes, b"not audio").expect("write notes.txt");
        lines.push(format!("file://{}", notes.display()));
        lines.push(format!("file://{}", extra.join("gone.wav").display()));

        // The list paste only fires while the list owns the keys and has
        // something selected, which is the state a user pasting into it is in.
        harness.state_mut().test_list_select_all();
        harness.run_steps(2);
        harness.event(egui::Event::Paste(lines.join("\n")));
        harness.run_steps(6);

        assert_eq!(
            harness.state().test_visible_list_paths().len(),
            before + 2,
            "both pasted wavs should have been imported"
        );
        let toast = harness.state().test_toast_messages().join(" | ");
        assert!(
            toast.contains("Added 2"),
            "the paste should report what it did, got {toast:?}"
        );
        assert!(
            toast.contains("not audio") && toast.contains("not found"),
            "and what it turned away, got {toast:?}"
        );
    }

    #[test]
    fn pasting_the_same_paths_again_says_they_are_already_there() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        harness.run_steps(2);

        let path = harness
            .state()
            .test_visible_list_paths()
            .first()
            .map(|p| p.display().to_string())
            .expect("a row in the list");
        harness.state_mut().test_list_select_all();
        harness.run_steps(2);
        let before = harness.state().test_visible_list_paths().len();
        harness.event(egui::Event::Paste(path));
        harness.run_steps(6);

        assert_eq!(harness.state().test_visible_list_paths().len(), before);
        let toast = harness.state().test_toast_messages().join(" | ");
        assert!(
            toast.contains("already in the list"),
            "a paste that adds nothing must say why, got {toast:?}"
        );
    }

    #[test]
    fn shift_z_fits_the_loop_region_to_the_view() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness.state_mut().test_set_loop_region_frac(0.40, 0.50));
        harness.run_steps(2);

        let tab_idx = harness.state().active_tab.expect("active tab");
        let (loop_start, loop_end) = harness.state().test_loop_region().expect("loop region");
        let spp_before = harness.state().tabs[tab_idx].samples_per_px;

        harness.key_press_modifiers(Modifiers::SHIFT, Key::Z);
        harness.run_steps(3);

        let tab = &harness.state().tabs[tab_idx];
        assert!(
            tab.samples_per_px < spp_before,
            "fitting a tenth of the file should zoom in: {} -> {}",
            spp_before,
            tab.samples_per_px
        );
        let visible = (tab.last_wave_w * tab.samples_per_px).ceil() as usize;
        let view_end = tab.view_offset + visible;
        assert!(
            tab.view_offset <= loop_start && view_end >= loop_end,
            "both loop edges should be on screen: view {}..{view_end} vs loop {loop_start}..{loop_end}",
            tab.view_offset
        );
    }

    #[test]
    fn shift_z_does_nothing_without_a_loop() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        harness.run_steps(2);
        let tab_idx = harness.state().active_tab.expect("active tab");
        assert!(harness.state().test_loop_region().is_none());
        let spp_before = harness.state().tabs[tab_idx].samples_per_px;

        harness.key_press_modifiers(Modifiers::SHIFT, Key::Z);
        harness.run_steps(3);

        assert_eq!(
            harness.state().tabs[tab_idx].samples_per_px,
            spp_before,
            "no loop, no zoom -- and Z must not have taken the press instead"
        );
    }

    #[test]
    fn double_clicking_a_loop_handle_zooms_in_around_it() {
        let mut harness = harness_with_editor_fixture();
        wait_for_scan(&mut harness);
        ensure_editor_ready(&mut harness);
        assert!(harness.state_mut().test_set_active_tool(ToolKind::LoopEdit));
        assert!(harness.state_mut().test_clear_markers());
        assert!(harness.state_mut().test_set_loop_region_frac(0.30, 0.70));
        harness.run_steps(2);

        let tab_idx = harness.state().active_tab.expect("active tab");
        let before = harness.state().test_loop_region().expect("loop region");
        let spp_before = harness.state().tabs[tab_idx].samples_per_px;

        let handle = editor_pos_at_loop_edge(&harness, before.0);
        double_click_at(&mut harness, handle);

        assert!(
            harness.state().tabs[tab_idx].samples_per_px < spp_before,
            "a double click on the handle should zoom in"
        );
        assert_eq!(
            harness.state().test_loop_region(),
            Some(before),
            "and must not move the loop while doing it"
        );
    }
}
