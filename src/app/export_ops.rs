use std::path::{Path, PathBuf};

use super::types::{ConflictPolicy, ExportResult, ExportState, MediaSource, SaveMode, VirtualSourceRef};

impl super::WavesPreviewer {
    fn resolve_virtual_export_parent(&self, item: &super::types::MediaItem) -> Option<PathBuf> {
        let mut current = item.virtual_state.as_ref().map(|v| v.source.clone())?;
        for _ in 0..8 {
            match current {
                VirtualSourceRef::FilePath(path) => {
                    return path.parent().map(|p| p.to_path_buf());
                }
                VirtualSourceRef::VirtualPath(path) => {
                    let next = self
                        .item_for_path(&path)
                        .and_then(|it| it.virtual_state.as_ref())
                        .map(|v| v.source.clone());
                    if let Some(next) = next {
                        current = next;
                        continue;
                    }
                    return None;
                }
                VirtualSourceRef::Sidecar(path) => {
                    let p = PathBuf::from(path);
                    return p.parent().map(|p| p.to_path_buf());
                }
            }
        }
        None
    }

    fn overwrite_backup_path(src: &Path) -> PathBuf {
        let fname = src.file_name().and_then(|s| s.to_str()).unwrap_or("backup");
        src.with_file_name(format!("{}.bak", fname))
    }

    pub(super) fn spawn_export_gains(&mut self, _overwrite: bool) {
        use std::sync::mpsc;
        let mut targets: Vec<(PathBuf, f32)> = Vec::new();
        for item in &self.items {
            if item.pending_gain_db.abs() > 0.0001 {
                targets.push((item.path.clone(), item.pending_gain_db));
            }
        }
        if targets.is_empty() {
            return;
        }
        let (tx, rx) = mpsc::channel::<ExportResult>();
        std::thread::spawn(move || {
            let mut ok = 0usize;
            let mut failed = 0usize;
            let mut success_paths = Vec::new();
            let mut failed_paths = Vec::new();
            for (src, db) in targets {
                let stem = src.file_stem().and_then(|s| s.to_str()).unwrap_or("out");
                let ext = src.extension().and_then(|s| s.to_str()).unwrap_or("");
                let dst = if ext.is_empty() {
                    src.with_file_name(format!("{} (gain{:+.1}dB)", stem, db))
                } else {
                    src.with_file_name(format!("{} (gain{:+.1}dB).{}", stem, db, ext))
                };
                match crate::wave::export_gain_audio(&src, &dst, db) {
                    Ok(_) => {
                        ok += 1;
                        success_paths.push(dst);
                    }
                    Err(e) => {
                        eprintln!("export failed {}: {e:?}", src.display());
                        failed += 1;
                        failed_paths.push(src.clone());
                    }
                }
            }
            let _ = tx.send(ExportResult {
                ok,
                failed,
                success_paths,
                failed_paths,
            });
        });
        self.export_state = Some(ExportState {
            msg: "Exporting gains".into(),
            rx,
        });
    }

    pub(super) fn spawn_save_selected(&mut self, indices: std::collections::BTreeSet<usize>) {
        use std::sync::mpsc;
        if indices.is_empty() {
            return;
        }
        struct EditSaveTask {
            src: PathBuf,
            audio: Option<std::sync::Arc<crate::audio::AudioBuffer>>,
            gain_db: f32,
            out_sr: u32,
            target_sr: u32,
            file_sr: u32,
            wav_bit_depth: Option<crate::wave::WavBitDepth>,
            max_file_samples: Option<u64>,
            markers: Vec<crate::markers::MarkerEntry>,
            loop_region: Option<(usize, usize)>,
            write_audio: bool,
            write_markers: bool,
            write_loop_markers: bool,
            format_override: Option<String>,
        }
        let cfg = self.export_cfg.clone();
        let format_override = cfg
            .format_override
            .as_ref()
            .map(|s| s.trim().to_ascii_lowercase())
            .filter(|s| crate::audio_io::is_supported_extension(s));
        let out_sr = self.audio.shared.out_sample_rate.max(1);
        let mut items: Vec<(PathBuf, f32)> = Vec::new();
        let mut edit_tasks: Vec<EditSaveTask> = Vec::new();
        let mut edit_sources: Vec<PathBuf> = Vec::new();
        let mut virtual_tasks: Vec<(
            PathBuf,
            PathBuf,
            std::sync::Arc<crate::audio::AudioBuffer>,
            f32,
            u32,
            u32,
        )> = Vec::new();
        for i in indices {
            let Some(item) = self.item_for_row(i) else {
                continue;
            };
            let p = item.path.clone();
            let db = item.pending_gain_db;
            let path_format_override = self
                .format_override
                .get(&p)
                .map(|v| v.trim().trim_start_matches('.').to_ascii_lowercase())
                .filter(|v| crate::audio_io::is_supported_extension(v));
            if item.source == MediaSource::Virtual {
                let audio = self
                    .edited_audio_for_path(&p)
                    .or_else(|| item.virtual_audio.clone());
                let Some(audio) = audio else {
                    continue;
                };
                let parent = self
                    .export_cfg
                    .dest_folder
                    .clone()
                    .or_else(|| self.resolve_virtual_export_parent(item))
                    .or_else(|| self.root.clone())
                    .unwrap_or_else(|| PathBuf::from("."));
                if let Err(err) = std::fs::create_dir_all(&parent) {
                    eprintln!(
                        "virtual export: failed to ensure output folder {}: {err:?}",
                        parent.display()
                    );
                }
                let display_name = item.display_name.clone();
                let stem = std::path::Path::new(&display_name)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("out");
                let mut name = self.export_cfg.name_template.clone();
                name = name.replace("{name}", stem);
                name = name.replace("{gain:+.1}", &format!("{:+.1}", db));
                name = name.replace("{gain:+0.0}", &format!("{:+.1}", db));
                name = name.replace("{gain}", &format!("{:+.1}", db));
                let name = crate::app::helpers::sanitize_filename_component(&name);
                let mut dst = parent.join(name);
                let target_ext = path_format_override
                    .as_deref()
                    .or(format_override.as_deref())
                    .unwrap_or("wav");
                dst.set_extension(target_ext);
                if dst.exists() {
                    match self.export_cfg.conflict {
                        ConflictPolicy::Overwrite => {}
                        ConflictPolicy::Skip => continue,
                        ConflictPolicy::Rename => {
                            let orig = dst.clone();
                            let orig_ext = orig
                                .extension()
                                .and_then(|e| e.to_str())
                                .unwrap_or(target_ext);
                            let mut idx = 1u32;
                            loop {
                                let stem2 =
                                    orig.file_stem().and_then(|s| s.to_str()).unwrap_or("out");
                                let n = crate::app::helpers::sanitize_filename_component(&format!(
                                    "{}_{:02}",
                                    stem2, idx
                                ));
                                dst = orig.with_file_name(n);
                                if !orig_ext.is_empty() {
                                    dst.set_extension(orig_ext);
                                }
                                if !dst.exists() {
                                    break;
                                }
                                idx += 1;
                                if idx > 999 {
                                    break;
                                }
                            }
                        }
                    }
                }
                let sr = item
                    .virtual_state
                    .as_ref()
                    .map(|v| v.sample_rate)
                    .or_else(|| item.meta.as_ref().map(|m| m.sample_rate))
                    .unwrap_or(self.audio.shared.out_sample_rate);
                let target_sr = self
                    .sample_rate_override
                    .get(&p)
                    .copied()
                    .unwrap_or(sr)
                    .max(1);
                virtual_tasks.push((p, dst, audio, db, sr.max(1), target_sr));
            } else {
                let mut dirty_audio = false;
                let mut markers_dirty = false;
                let mut loop_markers_dirty = false;
                let mut markers: Vec<crate::markers::MarkerEntry> = Vec::new();
                let mut loop_region: Option<(usize, usize)> = None;
                let mut ch_samples: Option<Vec<Vec<f32>>> = None;
                let mut max_file_samples: Option<u64> = None;
                let sr_override = self.sample_rate_override.get(&p).copied();
                let bit_override = self.bit_depth_override.get(&p).copied();
                if let Some(tab) = self.tabs.iter().find(|t| t.path.as_path() == p.as_path()) {
                    dirty_audio = tab.dirty;
                    markers_dirty = tab.markers_dirty;
                    loop_markers_dirty = tab.loop_markers_dirty;
                    markers = tab.markers_committed.clone();
                    loop_region = tab.loop_region_committed;
                    if dirty_audio
                        || markers_dirty
                        || loop_markers_dirty
                        || sr_override.is_some()
                        || bit_override.is_some()
                        || path_format_override.is_some()
                    {
                        let needs_audio = cfg.save_mode == SaveMode::NewFile
                            || dirty_audio
                            || db.abs() > 0.0001
                            || sr_override.is_some()
                            || bit_override.is_some()
                            || path_format_override.is_some();
                        if needs_audio {
                            let tab_ready = tab.samples_len > 0
                                && !tab.ch_samples.is_empty()
                                && tab.ch_samples[0].len() > 0;
                            if tab_ready {
                                ch_samples = Some(tab.ch_samples.clone());
                                let target_sr = sr_override.unwrap_or(out_sr);
                                let scaled =
                                    (tab.samples_len as f64) * (target_sr as f64 / out_sr as f64);
                                max_file_samples = Some(scaled.round().max(0.0) as u64);
                            } else {
                                max_file_samples = self
                                    .meta_for_path(&p)
                                    .and_then(|m| m.duration_secs)
                                    .map(|secs| {
                                        (secs * self.sample_rate_for_path(&p, out_sr) as f32)
                                            .round()
                                            .max(0.0) as u64
                                    });
                            }
                        } else {
                            max_file_samples = self
                                .meta_for_path(&p)
                                .and_then(|m| m.duration_secs)
                                .map(|secs| {
                                    (secs * self.sample_rate_for_path(&p, out_sr) as f32)
                                        .round()
                                        .max(0.0) as u64
                                });
                        }
                    }
                } else if let Some(cached) = self.edited_cache.get(&p) {
                    dirty_audio = cached.dirty;
                    markers_dirty = cached.markers_dirty;
                    loop_markers_dirty = cached.loop_markers_dirty;
                    markers = cached.markers_committed.clone();
                    loop_region = cached.loop_region_committed;
                    if dirty_audio
                        || markers_dirty
                        || loop_markers_dirty
                        || sr_override.is_some()
                        || bit_override.is_some()
                        || path_format_override.is_some()
                    {
                        let needs_audio = cfg.save_mode == SaveMode::NewFile
                            || dirty_audio
                            || db.abs() > 0.0001
                            || sr_override.is_some()
                            || bit_override.is_some()
                            || path_format_override.is_some();
                        if needs_audio {
                            let cache_ready = cached.samples_len > 0
                                && !cached.ch_samples.is_empty()
                                && cached.ch_samples[0].len() > 0;
                            if cache_ready {
                                ch_samples = Some(cached.ch_samples.clone());
                                let target_sr = sr_override.unwrap_or(out_sr);
                                let scaled = (cached.samples_len as f64)
                                    * (target_sr as f64 / out_sr as f64);
                                max_file_samples = Some(scaled.round().max(0.0) as u64);
                            } else {
                                max_file_samples = self
                                    .meta_for_path(&p)
                                    .and_then(|m| m.duration_secs)
                                    .map(|secs| {
                                        (secs * self.sample_rate_for_path(&p, out_sr) as f32)
                                            .round()
                                            .max(0.0) as u64
                                    });
                            }
                        } else {
                            max_file_samples = self
                                .meta_for_path(&p)
                                .and_then(|m| m.duration_secs)
                                .map(|secs| {
                                    (secs * self.sample_rate_for_path(&p, out_sr) as f32)
                                        .round()
                                        .max(0.0) as u64
                                });
                        }
                    }
                }
                let has_edits = dirty_audio
                    || markers_dirty
                    || loop_markers_dirty
                    || sr_override.is_some()
                    || bit_override.is_some()
                    || path_format_override.is_some();
                if has_edits {
                    let write_audio = cfg.save_mode == SaveMode::NewFile
                        || dirty_audio
                        || db.abs() > 0.0001
                        || sr_override.is_some()
                        || bit_override.is_some()
                        || path_format_override.is_some();
                    let audio = ch_samples
                        .map(crate::audio::AudioBuffer::from_channels)
                        .map(std::sync::Arc::new);
                    let target_sr = sr_override.unwrap_or(out_sr).max(1);
                    let file_sr = if write_audio {
                        target_sr
                    } else {
                        self.sample_rate_for_path(&p, out_sr)
                    };
                    let write_markers = markers_dirty || (write_audio && !markers.is_empty());
                    let write_loop_markers =
                        loop_markers_dirty || (write_audio && loop_region.is_some());
                    edit_tasks.push(EditSaveTask {
                        src: p.clone(),
                        audio,
                        gain_db: db,
                        out_sr,
                        target_sr,
                        file_sr,
                        wav_bit_depth: bit_override,
                        max_file_samples,
                        markers,
                        loop_region,
                        write_audio,
                        write_markers,
                        write_loop_markers,
                        format_override: path_format_override,
                    });
                    edit_sources.push(p);
                } else if db.abs() > 0.0001 {
                    items.push((p, db));
                }
            }
        }
        if items.is_empty() && edit_tasks.is_empty() && virtual_tasks.is_empty() {
            return;
        }
        let save_mode = if items.is_empty() && edit_tasks.is_empty() {
            SaveMode::NewFile
        } else {
            cfg.save_mode
        };
        self.saving_format_targets = if matches!(save_mode, SaveMode::Overwrite) {
            edit_tasks
                .iter()
                .filter_map(|task| {
                    let forced_ext = task
                        .format_override
                        .as_deref()
                        .or(format_override.as_deref())?;
                    let src_ext = task
                        .src
                        .extension()
                        .and_then(|e| e.to_str())
                        .unwrap_or("")
                        .to_ascii_lowercase();
                    if src_ext == forced_ext {
                        return None;
                    }
                    let mut dst = task.src.clone();
                    dst.set_extension(forced_ext);
                    Some((task.src.clone(), dst))
                })
                .collect()
        } else {
            Vec::new()
        };
        // remember sources for post-save cleanup + reload
        self.saving_sources = items.iter().map(|(p, _)| p.clone()).collect();
        self.saving_sources.extend(edit_sources.clone());
        self.saving_edit_sources = edit_sources;
        self.saving_virtual = virtual_tasks
            .iter()
            .map(|(src, dst, _, _, _, _)| (src.clone(), dst.clone()))
            .collect();
        self.saving_mode = Some(save_mode);
        let virtual_jobs = virtual_tasks
            .iter()
            .map(|(src, dst, audio, db, sr, target_sr)| {
                (
                    src.clone(),
                    dst.clone(),
                    audio.clone(),
                    *db,
                    *sr,
                    *target_sr,
                )
            })
            .collect::<Vec<_>>();
        let edit_jobs = edit_tasks;
        // File export prioritizes output quality over realtime speed.
        let resample_quality = crate::wave::ResampleQuality::Best;
        let (tx, rx) = mpsc::channel::<ExportResult>();
        std::thread::spawn(move || {
            let mut ok = 0usize;
            let mut failed = 0usize;
            let mut success_paths = Vec::new();
            let mut failed_paths = Vec::new();
            for (src, db) in items {
                match save_mode {
                    SaveMode::Overwrite => {
                        match crate::wave::overwrite_gain_audio(&src, db, cfg.backup_bak) {
                            Ok(()) => {
                                ok += 1;
                                success_paths.push(src.clone());
                            }
                            Err(_) => {
                                failed += 1;
                                failed_paths.push(src.clone());
                            }
                        }
                    }
                    SaveMode::NewFile => {
                        let parent = cfg.dest_folder.clone().unwrap_or_else(|| {
                            src.parent()
                                .unwrap_or_else(|| std::path::Path::new("."))
                                .to_path_buf()
                        });
                        let stem = src.file_stem().and_then(|s| s.to_str()).unwrap_or("out");
                        let mut name = cfg.name_template.clone();
                        name = name.replace("{name}", stem);
                        name = name.replace("{gain:+.1}", &format!("{:+.1}", db));
                        name = name.replace("{gain:+0.0}", &format!("{:+.1}", db));
                        name = name.replace("{gain}", &format!("{:+.1}", db));
                        let name = crate::app::helpers::sanitize_filename_component(&name);
                        let mut dst = parent.join(name);
                        let src_ext = src.extension().and_then(|e| e.to_str()).unwrap_or("wav");
                        let forced_ext = format_override.as_deref();
                        let dst_ext = dst.extension().and_then(|e| e.to_str());
                        let use_dst_ext = dst_ext
                            .map(|e| crate::audio_io::is_supported_extension(e))
                            .unwrap_or(false);
                        if let Some(ext) = forced_ext {
                            dst.set_extension(ext);
                        } else if !use_dst_ext {
                            dst.set_extension(src_ext);
                        }
                        if dst.exists() {
                            match cfg.conflict {
                                ConflictPolicy::Overwrite => {}
                                ConflictPolicy::Skip => {
                                    failed += 1;
                                    failed_paths.push(src.clone());
                                    continue;
                                }
                                ConflictPolicy::Rename => {
                                    let orig = dst.clone();
                                    let orig_ext =
                                        orig.extension().and_then(|e| e.to_str()).unwrap_or("");
                                    let mut idx = 1u32;
                                    loop {
                                        let stem2 = orig
                                            .file_stem()
                                            .and_then(|s| s.to_str())
                                            .unwrap_or("out");
                                        let n = crate::app::helpers::sanitize_filename_component(
                                            &format!("{}_{:02}", stem2, idx),
                                        );
                                        dst = orig.with_file_name(n);
                                        if !orig_ext.is_empty() {
                                            dst.set_extension(orig_ext);
                                        }
                                        if !dst.exists() {
                                            break;
                                        }
                                        idx += 1;
                                        if idx > 999 {
                                            break;
                                        }
                                    }
                                }
                            }
                        }
                        match crate::wave::export_gain_audio(&src, &dst, db) {
                            Ok(()) => {
                                ok += 1;
                                success_paths.push(dst.clone());
                            }
                            Err(_) => {
                                failed += 1;
                                failed_paths.push(src.clone());
                            }
                        }
                    }
                }
            }
            for task in edit_jobs {
                let dst = match save_mode {
                    SaveMode::Overwrite => {
                        let mut dst = task.src.clone();
                        if let Some(ext) = task
                            .format_override
                            .as_deref()
                            .or(format_override.as_deref())
                        {
                            dst.set_extension(ext);
                        }
                        dst
                    }
                    SaveMode::NewFile => {
                        let parent = cfg.dest_folder.clone().unwrap_or_else(|| {
                            task.src
                                .parent()
                                .unwrap_or_else(|| std::path::Path::new("."))
                                .to_path_buf()
                        });
                        let stem = task
                            .src
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or("out");
                        let mut name = cfg.name_template.clone();
                        name = name.replace("{name}", stem);
                        name = name.replace("{gain:+.1}", &format!("{:+.1}", task.gain_db));
                        name = name.replace("{gain:+0.0}", &format!("{:+.1}", task.gain_db));
                        name = name.replace("{gain}", &format!("{:+.1}", task.gain_db));
                        let name = crate::app::helpers::sanitize_filename_component(&name);
                        let mut dst = parent.join(name);
                        let src_ext = task
                            .src
                            .extension()
                            .and_then(|e| e.to_str())
                            .unwrap_or("wav");
                        let forced_ext = task
                            .format_override
                            .as_deref()
                            .or(format_override.as_deref());
                        let dst_ext = dst.extension().and_then(|e| e.to_str());
                        let use_dst_ext = dst_ext
                            .map(|e| crate::audio_io::is_supported_extension(e))
                            .unwrap_or(false);
                        if let Some(ext) = forced_ext {
                            dst.set_extension(ext);
                        } else if !use_dst_ext {
                            dst.set_extension(src_ext);
                        }
                        if dst.exists() {
                            match cfg.conflict {
                                ConflictPolicy::Overwrite => {}
                                ConflictPolicy::Skip => {
                                    failed += 1;
                                    failed_paths.push(task.src.clone());
                                    continue;
                                }
                                ConflictPolicy::Rename => {
                                    let orig = dst.clone();
                                    let orig_ext =
                                        orig.extension().and_then(|e| e.to_str()).unwrap_or("");
                                    let mut idx = 1u32;
                                    loop {
                                        let stem2 = orig
                                            .file_stem()
                                            .and_then(|s| s.to_str())
                                            .unwrap_or("out");
                                        let n = crate::app::helpers::sanitize_filename_component(
                                            &format!("{}_{:02}", stem2, idx),
                                        );
                                        dst = orig.with_file_name(n);
                                        if !orig_ext.is_empty() {
                                            dst.set_extension(orig_ext);
                                        }
                                        if !dst.exists() {
                                            break;
                                        }
                                        idx += 1;
                                        if idx > 999 {
                                            break;
                                        }
                                    }
                                }
                            }
                        }
                        dst
                    }
                };
                let mut max_file_samples = task.max_file_samples;
                if task.write_audio {
                    let (mut channels, src_sr) = if let Some(audio) = task.audio.as_ref() {
                        (audio.channels.clone(), task.out_sr)
                    } else {
                        match crate::audio_io::decode_audio_multi(&task.src) {
                            Ok((decoded, decoded_sr)) => (decoded, decoded_sr.max(1)),
                            Err(_) => {
                                failed += 1;
                                failed_paths.push(task.src.clone());
                                continue;
                            }
                        }
                    };
                    if task.gain_db.abs() > 0.0001 {
                        let gain = 10.0f32.powf(task.gain_db / 20.0);
                        for ch in channels.iter_mut() {
                            for v in ch.iter_mut() {
                                *v *= gain;
                            }
                        }
                    }
                    if src_sr != task.target_sr {
                        for ch in channels.iter_mut() {
                            *ch = crate::wave::resample_quality(
                                ch,
                                src_sr,
                                task.target_sr,
                                resample_quality,
                            );
                        }
                    }
                    let res = match save_mode {
                        SaveMode::Overwrite => {
                            crate::wave::overwrite_audio_from_channels_with_depth(
                                &channels,
                                task.target_sr,
                                &dst,
                                cfg.backup_bak,
                                task.wav_bit_depth,
                            )
                        }
                        SaveMode::NewFile => crate::wave::export_channels_audio_with_depth(
                            &channels,
                            task.target_sr,
                            &dst,
                            task.wav_bit_depth,
                        ),
                    };
                    if res.is_err() {
                        failed += 1;
                        failed_paths.push(task.src.clone());
                        continue;
                    }
                    max_file_samples =
                        max_file_samples.or_else(|| channels.get(0).map(|c| c.len() as u64));
                } else if !task.src.is_file() {
                    failed += 1;
                    failed_paths.push(task.src.clone());
                    continue;
                }
                let mut marker_ok = true;
                if task.write_markers {
                    if let Err(err) = crate::markers::write_markers(
                        &dst,
                        task.out_sr,
                        task.file_sr,
                        &task.markers,
                    ) {
                        eprintln!("write markers failed {}: {err:?}", dst.display());
                        marker_ok = false;
                    }
                }
                if task.write_loop_markers {
                    let mut loop_opt: Option<(u64, u64)> = None;
                    if let Some((s, e)) = task.loop_region {
                        if let Some((mut ls, mut le)) = crate::wave::map_loop_markers_to_file_sr(
                            s,
                            e,
                            task.out_sr,
                            task.file_sr,
                        ) {
                            if let Some(max) = max_file_samples {
                                if max > 0 {
                                    let max = max.min(u32::MAX as u64);
                                    ls = (ls as u64).min(max) as u32;
                                    le = (le as u64).min(max) as u32;
                                }
                            }
                            if le > ls {
                                loop_opt = Some((ls as u64, le as u64));
                            }
                        }
                    }
                    if let Err(err) = crate::loop_markers::write_loop_markers(&dst, loop_opt) {
                        eprintln!("write loop markers failed {}: {err:?}", dst.display());
                        marker_ok = false;
                    }
                }
                if marker_ok {
                    ok += 1;
                    success_paths.push(dst.clone());
                } else {
                    failed += 1;
                    failed_paths.push(dst.clone());
                }
            }
            for (_src, dst, audio, db, sr, target_sr) in virtual_jobs {
                let mut channels = audio.channels.clone();
                if db.abs() > 0.0001 {
                    let gain = 10.0f32.powf(db / 20.0);
                    for ch in channels.iter_mut() {
                        for v in ch.iter_mut() {
                            *v *= gain;
                        }
                    }
                }
                let mut out_sr = sr.max(1);
                if out_sr != target_sr.max(1) {
                    for ch in channels.iter_mut() {
                        *ch = crate::wave::resample_quality(
                            ch,
                            out_sr,
                            target_sr.max(1),
                            resample_quality,
                        );
                    }
                    out_sr = target_sr.max(1);
                }
                let res = crate::wave::export_channels_audio(&channels, out_sr, &dst);
                match res {
                    Ok(()) => {
                        ok += 1;
                        success_paths.push(dst.clone());
                    }
                    Err(err) => {
                        eprintln!(
                            "virtual export failed {}: sr={} ch={} err={:?}",
                            dst.display(),
                            out_sr,
                            channels.len(),
                            err
                        );
                        failed += 1;
                        failed_paths.push(dst.clone());
                    }
                }
            }
            let _ = tx.send(ExportResult {
                ok,
                failed,
                success_paths,
                failed_paths,
            });
        });
        self.export_state = Some(ExportState {
            msg: "Saving...".into(),
            rx,
        });
    }

    pub(super) fn spawn_convert_bits_selected(
        &mut self,
        paths: Vec<PathBuf>,
        depth: crate::wave::WavBitDepth,
    ) {
        let mut targets: Vec<PathBuf> = Vec::new();
        for p in paths {
            let is_wav = p
                .extension()
                .and_then(|s| s.to_str())
                .map(|s| s.eq_ignore_ascii_case("wav"))
                .unwrap_or(false);
            if p.is_file() && is_wav {
                targets.push(p);
            }
        }
        if targets.is_empty() {
            return;
        }
        let before = self.capture_list_selection_snapshot();
        let before_items = self.capture_list_undo_items_by_paths(&targets);
        for p in &targets {
            let file_bits = self
                .meta_for_path(p)
                .map(|m| m.bits_per_sample)
                .filter(|v| *v > 0)
                .or_else(|| {
                    crate::audio_io::read_audio_info(p)
                        .ok()
                        .map(|info| info.bits_per_sample)
                })
                .unwrap_or(0);
            if file_bits == depth.bits_per_sample() {
                self.bit_depth_override.remove(p);
            } else {
                self.bit_depth_override.insert(p.clone(), depth);
            }
        }
        self.record_list_update_from_paths(&targets, before_items, before);
        self.refresh_audio_after_sample_rate_change(&targets);
    }

    pub(super) fn spawn_convert_format_selected(&mut self, paths: Vec<PathBuf>, target_ext: &str) {
        let ext = target_ext.trim().to_ascii_lowercase();
        if !crate::audio_io::is_supported_extension(&ext) {
            return;
        }
        let mut targets: Vec<PathBuf> = Vec::new();
        for path in paths {
            if self.row_for_path(&path).is_some() {
                targets.push(path);
            }
        }
        if targets.is_empty() {
            return;
        }
        targets.sort();
        targets.dedup();
        let before = self.capture_list_selection_snapshot();
        let before_items = self.capture_list_undo_items_by_paths(&targets);
        for path in &targets {
            let src_ext = path
                .extension()
                .and_then(|s| s.to_str())
                .map(|s| s.to_ascii_lowercase())
                .unwrap_or_default();
            if src_ext == ext {
                self.format_override.remove(path);
            } else {
                self.format_override.insert(path.clone(), ext.clone());
            }
            self.refresh_display_name_for_path(path);
        }
        self.record_list_update_from_paths(&targets, before_items, before);
        self.refresh_audio_after_sample_rate_change(&targets);
    }

    pub(super) fn drain_export_results(&mut self, ctx: &egui::Context) {
        let Some(state) = &self.export_state else {
            return;
        };
        if let Ok(res) = state.rx.try_recv() {
            eprintln!("save/export done: ok={}, failed={}", res.ok, res.failed);
            if state.msg.starts_with("Saving") {
                let sources = self.saving_sources.clone();
                let edit_sources = self.saving_edit_sources.clone();
                for p in &sources {
                    self.set_pending_gain_db_for_path(p, 0.0);
                    self.lufs_override.remove(p);
                    self.sample_rate_override.remove(p);
                    self.sample_rate_probe_cache.remove(p);
                    self.bit_depth_override.remove(p);
                    self.format_override.remove(p);
                    self.refresh_display_name_for_path(p);
                }
                let success_set: std::collections::HashSet<PathBuf> =
                    res.success_paths.iter().cloned().collect();
                if matches!(self.saving_mode, Some(SaveMode::Overwrite))
                    && self.export_cfg.backup_bak
                {
                    let mut restore_batch: Vec<(PathBuf, PathBuf)> = Vec::new();
                    for src in &self.saving_sources {
                        if !success_set.contains(src) {
                            continue;
                        }
                        let bak = Self::overwrite_backup_path(src);
                        if bak.is_file() {
                            restore_batch.push((src.clone(), bak));
                        }
                    }
                    if !restore_batch.is_empty() {
                        self.overwrite_undo_stack.push(restore_batch);
                        while self.overwrite_undo_stack.len() > 20 {
                            self.overwrite_undo_stack.remove(0);
                        }
                    }
                }
                if matches!(self.saving_mode, Some(SaveMode::Overwrite)) {
                    for p in &edit_sources {
                        if success_set.contains(p) {
                            self.mark_edit_saved_for_path(p);
                        }
                    }
                }
                let mut format_success: Vec<(PathBuf, PathBuf)> = Vec::new();
                for (src, dst) in &self.saving_format_targets {
                    if success_set.contains(dst) {
                        format_success.push((src.clone(), dst.clone()));
                    }
                }
                for (src, dst) in &format_success {
                    self.mark_edit_saved_for_path(src);
                    self.replace_path_in_state(src, dst);
                    self.sample_rate_override.remove(dst);
                    self.sample_rate_probe_cache.remove(dst);
                    self.bit_depth_override.remove(dst);
                    self.format_override.remove(dst);
                    self.refresh_display_name_for_path(dst);
                }
                let mut virtual_success: Vec<(PathBuf, PathBuf)> = Vec::new();
                for (src, dst) in &self.saving_virtual {
                    if success_set.contains(dst) {
                        virtual_success.push((src.clone(), dst.clone()));
                    }
                }
                for (src, dst) in &virtual_success {
                    self.set_pending_gain_db_for_path(src, 0.0);
                    self.lufs_override.remove(src);
                    self.sample_rate_override.remove(src);
                    self.sample_rate_probe_cache.remove(src);
                    self.bit_depth_override.remove(src);
                    self.format_override.remove(src);
                    self.replace_path_in_state(src, dst);
                    self.sample_rate_override.remove(dst);
                    self.sample_rate_probe_cache.remove(dst);
                    self.bit_depth_override.remove(dst);
                    self.format_override.remove(dst);
                }
                match self.saving_mode.unwrap_or(self.export_cfg.save_mode) {
                    SaveMode::Overwrite => {
                        let mut reload_paths = self.saving_sources.clone();
                        for (_src, dst) in &format_success {
                            reload_paths.push(dst.clone());
                        }
                        reload_paths.sort();
                        reload_paths.dedup();
                        if !reload_paths.is_empty() {
                            self.ensure_meta_pool();
                            for p in reload_paths.iter() {
                                self.clear_meta_for_path(&p);
                                self.meta_inflight.remove(p);
                                self.queue_meta_for_path(p, false);
                            }
                        }
                        if let Some(path) = format_success
                            .first()
                            .map(|(_, dst)| dst.clone())
                            .or_else(|| self.saving_sources.get(0).cloned())
                        {
                            if let Some(idx) = self.row_for_path(&path) {
                                self.select_and_load(idx, true);
                            }
                        }
                    }
                    SaveMode::NewFile => {
                        let virtual_dests: std::collections::HashSet<PathBuf> = self
                            .saving_virtual
                            .iter()
                            .map(|(_, dst)| dst.clone())
                            .collect();
                        let mut added_any = false;
                        let mut first_added = None;
                        for p in &res.success_paths {
                            if virtual_dests.contains(p) {
                                continue;
                            }
                            if self.add_files_merge(&[p.clone()]) > 0 {
                                if first_added.is_none() {
                                    first_added = Some(p.clone());
                                }
                                added_any = true;
                            }
                        }
                        if added_any {
                            self.after_add_refresh();
                        }
                        if let Some(p) = first_added {
                            if let Some(idx) = self.row_for_path(&p) {
                                self.select_and_load(idx, true);
                            }
                        }
                    }
                }
                self.saving_sources.clear();
                self.saving_virtual.clear();
                self.saving_format_targets.clear();
                self.saving_edit_sources.clear();
                self.saving_mode = None;
            }
            self.export_state = None;
            ctx.request_repaint();
        }
    }

    pub(super) fn undo_last_overwrite_export(&mut self) -> bool {
        let Some(batch) = self.overwrite_undo_stack.pop() else {
            return false;
        };
        let mut restored: Vec<PathBuf> = Vec::new();
        for (src, bak) in batch {
            if !bak.is_file() {
                continue;
            }
            let parent = src.parent().unwrap_or_else(|| Path::new("."));
            let ext = src.extension().and_then(|s| s.to_str()).unwrap_or("tmp");
            let tmp = parent.join(format!("._wvp_undo_tmp.{}", ext));
            if tmp.exists() {
                let _ = std::fs::remove_file(&tmp);
            }
            if std::fs::copy(&bak, &tmp).is_err() {
                let _ = std::fs::remove_file(&tmp);
                continue;
            }
            let _ = std::fs::remove_file(&src);
            if std::fs::rename(&tmp, &src).is_ok() {
                restored.push(src);
            } else {
                let _ = std::fs::remove_file(&tmp);
            }
        }
        if restored.is_empty() {
            return false;
        }
        self.ensure_meta_pool();
        for path in &restored {
            self.clear_meta_for_path(path);
            self.meta_inflight.remove(path);
            self.queue_meta_for_path(path, false);
        }
        if let Some(first) = restored.first().cloned() {
            if let Some(row) = self.row_for_path(&first) {
                self.select_and_load(row, true);
            }
        }
        true
    }
}
