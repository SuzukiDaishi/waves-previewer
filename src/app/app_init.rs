use crate::audio::AudioEngine;

use super::*;

impl WavesPreviewer {
    pub(super) fn build_app(startup: StartupConfig, audio: AudioEngine) -> Self {
        audio.set_loop_enabled(false);
        let metadata_cache_path = if cfg!(feature = "kittest") {
            std::env::var_os("NEOWAVES_METADATA_CACHE")
                .filter(|value| !value.is_empty())
                .map(std::path::PathBuf::from)
        } else {
            std::env::var_os("NEOWAVES_METADATA_CACHE")
                .filter(|value| !value.is_empty())
                .map(std::path::PathBuf::from)
                .or_else(crate::metadata::cache::default_cache_path)
        };
        let (metadata_summary_pool, metadata_summary_rx) =
            crate::metadata::cache::MetadataSummaryPool::new(metadata_cache_path);
        let ipc_rx = startup.ipc_rx.clone();
        let startup_state = StartupState::new(startup.clone());
        let debug_state = DebugState::new(startup.debug.clone());
        let mut app = Self {
            audio,
            root: None,
            items: Vec::new(),
            item_index: Default::default(),
            path_index: Default::default(),
            folder_intern: Default::default(),
            files: Vec::new(),
            next_media_id: 1,
            selected: None,
            volume_db: 0.0,
            audio_output_device_name: None,
            audio_output_devices: Vec::new(),
            audio_output_error: None,
            audio_channel_map_direct: false,
            audio_device_watch: AudioDeviceWatchState::default(),
            audio_bootstrap_rx: None,
            startup_paths_applied: false,
            startup_maintenance_step: 0,
            startup_maintenance_next_at: std::time::Instant::now()
                + std::time::Duration::from_millis(300),
            playback_rate: 1.0,
            playback_session: PlaybackSessionState::default(),
            playback_fx_state: None,
            playback_fx_job_id: 0,
            playback_source_generation: 0,
            playback_base_audio: None,
            prepared_playback_fx_audio: None,
            prepared_playback_fx_generation: 0,
            prepared_playback_fx_mode: None,
            prepared_playback_fx_rate: 1.0,
            prepared_playback_fx_pitch: 0.0,
            pitch_semitones: 0.0,
            meter_db: -80.0,
            meter_ch_db: Vec::new(),
            meter_ch_hold_db: Vec::new(),
            meter_ch_last_time: None,
            topbar_volume_rect: None,
            topbar_volume_last_click: None,
            editor_loop_handle_last_click: None,
            topbar_output_meter_rect: None,
            topbar_search_rect: None,
            editor_inspector_rect: None,
            tabs: Vec::new(),
            waveform_scratch: WaveformScratch::default(),
            active_tab: None,
            workspace_view: WorkspaceView::List,
            effect_graph: EffectGraphState::default(),
            meta_rx: None,
            meta_pool: None,
            meta_inflight: Default::default(),
            meta_sort_pending: false,
            meta_backlog_peak: 0,
            pending_gain_count_cache: None,
            editor_wave_cache_tx: None,
            editor_wave_cache_rx: None,
            editor_wave_cache_generation: HashMap::new(),
            editor_wave_cache_generation_counter: 0,
            session_save_state: None,
            clipboard_prep_state: None,
            world_f0_method: crate::app::types::WorldF0Method::Dio,
            meta_sort_last_applied: None,
            list_meta_prefetch_cursor: 0,
            transcript_inflight: HashSet::new(),
            transcript_ai_inflight: HashSet::new(),
            transcript_ai_source_aliases: HashMap::new(),
            show_transcript_window: false,
            pending_transcript_seek: None,
            transcript_ai_opt_in: false,
            transcript_ai_model_dir: None,
            transcript_ai_available: false,
            transcript_ai_state: None,
            transcript_model_download_state: None,
            transcript_ai_last_error: None,
            transcript_ai_cfg: TranscriptAiConfig::default(),
            transcript_supported_languages: Vec::new(),
            transcript_supported_tasks: Vec::new(),
            music_ai_model_dir: None,
            music_ai_available: false,
            music_ai_demucs_model_path: None,
            music_ai_state: None,
            music_model_download_state: None,
            music_ai_last_error: None,
            music_ai_inflight: HashSet::new(),
            music_preview_state: None,
            music_preview_generation_counter: 0,
            music_preview_expected_generation: 0,
            external_sources: Vec::new(),
            external_active_source: None,
            external_source: None,
            external_headers: Vec::new(),
            external_rows: Vec::new(),
            external_key_index: None,
            external_key_rule: ExternalKeyRule::FileName,
            external_match_input: ExternalRegexInput::FileName,
            external_visible_columns: Vec::new(),
            external_lookup: HashMap::new(),
            external_key_row_index: HashMap::new(),
            external_match_count: 0,
            external_unmatched_count: 0,
            external_show_unmatched: false,
            external_unmatched_rows: Vec::new(),
            external_sheet_names: Vec::new(),
            external_sheet_selected: None,
            external_has_header: true,
            external_header_row: None,
            external_data_row: None,
            external_scope_regex: String::new(),
            external_settings_dirty: false,
            show_external_dialog: false,
            external_load_error: None,
            external_match_regex: String::new(),
            external_match_replace: String::new(),
            external_load_rx: None,
            external_load_inflight: false,
            external_load_rows: 0,
            external_load_started_at: None,
            external_load_path: None,
            external_load_target: None,
            external_load_queue: VecDeque::new(),
            pending_external_restore: None,
            tool_defs: Vec::new(),
            tool_queue: std::collections::VecDeque::new(),
            tool_run_rx: None,
            tool_worker_busy: false,
            tool_log: std::collections::VecDeque::new(),
            tool_log_max: 200,
            show_tool_palette: false,
            tool_search: String::new(),
            tool_selected: None,
            tool_args_overrides: HashMap::new(),
            tool_config_error: None,
            pending_tool_confirm: None,
            spectro_cache: HashMap::new(),
            spectro_inflight: HashSet::new(),
            spectro_progress: HashMap::new(),
            spectro_cancel: HashMap::new(),
            spectro_cache_order: VecDeque::new(),
            spectro_cache_sizes: HashMap::new(),
            spectro_cache_bytes: 0,
            spectro_generation: HashMap::new(),
            spectro_generation_counter: 0,
            spectro_cfg: SpectrogramConfig::default(),
            spectro_tx: None,
            spectro_rx: None,
            headless: false,
            video_frame_tx: None,
            video_frame_rx: None,
            video_workers: Vec::new(),
            editor_viewport_tx: None,
            editor_viewport_rx: None,
            editor_viewport_generation_counter: 0,
            editor_feature_cache: HashMap::new(),
            editor_feature_inflight: HashSet::new(),
            editor_feature_progress: HashMap::new(),
            editor_feature_progress_shared: HashMap::new(),
            editor_feature_cancel: HashMap::new(),
            editor_feature_generation: HashMap::new(),
            editor_feature_generation_counter: 0,
            editor_feature_tx: None,
            editor_feature_rx: None,
            scan_rx: None,
            scan_pending_batches: VecDeque::new(),
            scan_in_progress: false,
            scan_worker_done: false,
            scan_started_at: None,
            scan_found_count: 0,
            scan_found_live: None,
            scan_visited_count: 0,
            scan_load_kind: None,
            scan_pending_target: None,
            wave_row_h: 26.0,
            list_columns: ListColumnConfig::default(),
            list_column_order: crate::app::types::ColumnId::ALL.to_vec(),
            list_column_layout: crate::app::types::ColumnId::ALL
                .iter()
                .copied()
                .map(crate::app::types::ColumnKey::Builtin)
                .collect(),
            list_columns_window_pos: None,
            list_columns_window_global_pos: None,
            metadata_list_columns: Vec::new(),
            metadata_summary_pool: Some(metadata_summary_pool),
            metadata_summary_rx: Some(metadata_summary_rx),
            metadata_summary_cache: Default::default(),
            metadata_summary_inflight: Default::default(),
            metadata_summary_generation: 0,
            metadata_summary_prefetch_cursor: 0,
            metadata_summary_errors: Default::default(),
            metadata_cache_hits: 0,
            list_col_widths: Default::default(),
            list_col_widths_seen: Vec::new(),
            list_table_ui_id: None,
            list_table_col_count: 0,
            list_table_layout_revision: 0,
            list_max_duration_secs: 0.0,
            list_art_textures: HashMap::new(),
            path_status: crate::app::path_status::PathStatusService::new(),
            search_highlight_cache: None,
            overlay_bins_cache: Default::default(),
            show_list_art_window: false,
            list_art_window_path: None,
            list_art_window_texture: None,
            list_art_window_error: None,
            selected_multi: std::collections::BTreeSet::new(),
            select_anchor: None,
            clipboard_payload: None,
            clipboard_temp_files: Vec::new(),
            pending_external_drag: None,
            external_drag_temp_files: VecDeque::new(),
            external_drag_last_status: None,
            recording_temp_files: Vec::new(),
            clipboard_c_was_down: false,
            editor_audio_clipboard: None,
            inspection_run_state: None,
            inspection_report: None,
            show_inspection_dialog: false,
            show_inspection_window: false,
            inspection_window_filter_idx: 0,
            batch_loudnorm_state: None,
            show_loudnorm_dialog: false,
            loudnorm_dialog_target: -14.0,
            inspection_cfg: Default::default(),
            editor_clip_c_was_down: false,
            editor_clip_x_was_down: false,
            editor_clip_v_was_down: false,
            clipboard_v_was_down: false,
            undo_z_was_down: false,
            list_undo_stack: Vec::new(),
            list_redo_stack: Vec::new(),
            last_undo_scope: UndoScope::Editor,
            sort_key: SortKey::File,
            sort_dir: SortDir::None,
            sort_loading_started_at: None,
            sort_loading_hold_until: None,
            sort_job: None,
            sort_rx: None,
            sort_request_seq: 0,
            filter_job: None,
            deferred_list_drop: Vec::new(),
            files_membership_revision: 0,
            sort_loading_last_ms: 0.0,
            list_scroll_row: 0,
            list_seek_pending: None,
            list_seek_gesture: None,
            list_decode_progress: None,
            list_preview_job_max_secs: 0.0,
            list_scroll_residual: 0.0,
            list_last_fully_visible_row: None,
            list_end_row_fully_visible: false,
            scroll_to_selected: false,
            last_list_scroll_at: None,
            auto_play_list_nav: false,
            list_stop_returns_to_start: false,
            list_click_audition: true,
            suppress_list_enter: false,
            list_has_focus: false,
            search_has_focus: false,
            ui_input_focus: Default::default(),
            input_scope: Default::default(),
            original_files: Vec::new(),
            search_query: String::new(),
            search_use_regex: false,
            search_dirty: false,
            search_deadline: None,
            skip_dotfiles: true,
            zero_cross_epsilon: 1.0e-4,
            blank_threshold_dbfs: crate::app::inspection::DEFAULT_BLANK_THRESHOLD_DBFS,
            blank_min_ms: crate::app::inspection::DEFAULT_BLANK_MIN_MS,
            spectral_edit_time_fade_ms: 8.0,
            spectral_edit_freq_fade_hz: 80.0,
            editor_play_selection_state: None,
            invert_wave_zoom_wheel: false,
            invert_shift_wheel_pan: false,
            editor_wheel_scrolls: false,
            horizontal_zoom_anchor_mode: EditorHorizontalZoomAnchorMode::Pointer,
            editor_horizontal_scroll_speed: EditorHorizontalScrollSpeed::default(),
            editor_pause_resume_mode: EditorPauseResumeMode::ReturnToLastStart,
            mode: RateMode::Speed,
            processing: None,
            processing_job_id: 0,
            list_preview_rx: None,
            list_preview_job_id: 0,
            list_preview_active_buffer_job: None,
            list_preview_job_epoch: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            list_preview_partial_ready: false,
            list_preview_pending_path: None,
            list_play_pending: false,
            variation_audition: None,
            mix_audition_state: None,
            variation_audition_advancing: false,
            duplicate_scan_state: None,
            duplicate_report: None,
            show_duplicates_window: false,
            folder_watch: None,
            watch_folder_enabled: true,
            watch_poll_interval_ms: 3_000,
            dup_allow_offset: true,
            show_engine_export_dialog: false,
            engine_export_profile: crate::app::engine_export::EngineProfile::Unity,
            show_bwf_dialog: false,
            bwf_fields: crate::wave::BextFields::default(),
            bwf_info: crate::wave::InfoFields::default(),
            bwf_ixml: crate::wave::IxmlFields::default(),
            list_preview_prefetch_tx: None,
            list_preview_prefetch_rx: None,
            list_preview_prefetch_inflight: HashSet::new(),
            list_preview_cache: HashMap::new(),
            list_preview_cache_order: VecDeque::new(),
            plugin_search_paths: Self::default_plugin_search_paths(),
            plugin_search_path_input: String::new(),
            plugin_preset_name_input: String::new(),
            show_plugin_manager: false,
            plugin_catalog: Self::load_cached_plugin_catalog(),
            plugin_scan_state: None,
            plugin_scan_error: None,
            plugin_probe_state: None,
            plugin_process_state: None,
            plugin_gui_state: None,
            plugin_rack_worker: std::sync::Arc::new(std::sync::Mutex::new(None)),
            plugin_job_id: 0,
            plugin_temp_seq: 0,
            perf: crate::app::perf_profile::PerfProfile::default(),
            frame_budget: crate::app::frame_budget::FrameBudget::default(),
            zoo_enabled: false,
            zoo_walk_enabled: true,
            zoo_voice_enabled: false,
            zoo_use_bpm: true,
            zoo_gif_path: None,
            zoo_voice_path: None,
            zoo_scale: 0.5,
            zoo_opacity: 1.0,
            zoo_speed: 140.0,
            zoo_flip_manual: false,
            zoo_anim_clock: 0.0,
            zoo_pos_x: 0.0,
            zoo_dir: 1.0,
            zoo_last_tick: std::time::Instant::now(),
            zoo_squish_until: None,
            zoo_last_error: None,
            zoo_texture_gen: 0,
            zoo_frames_raw: Vec::new(),
            zoo_frames_tex: Vec::new(),
            zoo_voice_cache_path: None,
            zoo_voice_cache: None,
            zoo_voice_audio: None,
            editor_apply_state: None,
            virtual_trim_state: None,
            virtual_trim_queue: std::collections::VecDeque::new(),
            editor_decode_state: None,
            editor_decode_job_id: 0,
            edited_cache: HashMap::new(),
            export_state: None,
            csv_export_state: None,
            playing_path: None,

            export_cfg: ExportConfig {
                first_prompt: true,
                save_mode: SaveMode::NewFile,
                dest_folder: None,
                name_template: "{name}{gain_suffix}".into(),
                format_override: None,
                conflict: ConflictPolicy::Rename,
                backup_bak: true,
                export_srt: false,
                codec: Default::default(),
            },
            show_export_settings: false,
            show_list_columns_window: false,
            show_shortcuts_window: false,
            show_keymap_window: false,
            show_undo_history_window: false,
            show_regions_window: false,
            scrub_state: None,
            world_ap_slider: 1.0,
            spectral_clipboard: None,
            harmonic_action: None,
            keymap_overrides: std::collections::HashMap::new(),
            keymap_capture: None,
            show_transcription_settings: false,
            show_first_save_prompt: false,
            project_path: None,
            session_path_mode: super::project::SessionPathMode::Absolute,
            recent_sessions: Vec::new(),
            project_open_pending: None,
            project_open_state: None,
            project_open_generation: 0,
            close_after_session_save: false,
            theme_mode: ThemeMode::Dark,
            item_bg_mode: ItemBgMode::Standard,
            show_rename_dialog: false,
            inline_rename_path: None,
            inline_rename_buffer: String::new(),
            inline_rename_focus_next: false,
            rename_target: None,
            rename_input: String::new(),
            rename_focus_next: false,
            rename_error: None,
            show_batch_rename_dialog: false,
            batch_rename_targets: Vec::new(),
            batch_rename_pattern: "{name}_{n}".into(),
            batch_rename_start: 1,
            batch_rename_pad: 2,
            batch_rename_error: None,
            saving_sources: Vec::new(),
            saving_virtual: Vec::new(),
            saving_format_targets: Vec::new(),
            saving_edit_sources: Vec::new(),
            saving_edit_annotations: HashMap::new(),
            saving_mode: None,
            overwrite_undo_stack: Vec::new(),

            lufs_override: HashMap::new(),
            lufs_recalc_deadline: HashMap::new(),
            lufs_rx2: None,
            lufs_worker_busy: false,
            sample_rate_override: HashMap::new(),
            sample_rate_probe_cache: Default::default(),
            bit_depth_override: HashMap::new(),
            format_override: HashMap::new(),
            src_quality: SrcQuality::Good,
            show_resample_dialog: false,
            resample_targets: Vec::new(),
            resample_target_sr: 48_000,
            resample_error: None,
            bulk_resample_state: None,
            leave_intent: None,
            show_leave_prompt: false,
            toasts: Vec::new(),
            last_seen_resample_fallbacks: 0,
            show_quit_prompt: false,
            force_quit: false,
            pending_activate_path: None,
            pending_activate_kind: None,
            pending_activate_ready: false,
            pending_editor_autoplay_path: None,
            pending_editor_playback_handoff: None,
            pending_preview_autoplay: None,
            heavy_preview_rx: None,
            heavy_preview_gen_counter: 0,
            heavy_preview_expected_gen: 0,
            heavy_preview_expected_path: None,
            heavy_preview_expected_tool: None,
            heavy_overlay_rx: None,
            overlay_gen_counter: 0,
            overlay_expected_gen: 0,
            overlay_expected_path: None,
            overlay_expected_tool: None,

            startup: startup_state,
            pending_screenshot: None,
            exit_after_screenshot: false,
            screenshot_seq: 0,

            debug: debug_state,
            debug_summary_seq: 0,
            crash_reports: CrashReportState::default(),
            ipc_rx,
            #[cfg(feature = "kittest")]
            test_dialogs: TestDialogQueue::default(),

            recording_tab: RecordingTabState::default(),
        };
        app.load_prefs();
        app.apply_audio_channel_map_mode();
        app.load_metadata_column_registry();
        app.sanitize_list_column_layout();
        app
    }

    pub(super) fn finish_test_startup(&mut self) {
        self.cleanup_neowaves_temp_cache_files();
        self.refresh_crash_reports_on_startup();
        self.refresh_audio_output_devices();
        if self.audio_output_device_name.is_some() {
            let preferred = self.audio_output_device_name.clone();
            let _ = self.apply_audio_output_device_selection(preferred, false);
        }
        self.refresh_transcript_ai_status();
        self.refresh_music_ai_status();
        self.reload_zoo_gif_frames();
        self.load_tools_config();
        self.load_effect_graph_library();
        self.revalidate_effect_graph_draft();
        self.apply_startup_paths();
        self.setup_debug_automation();
        self.startup_paths_applied = true;
        self.startup_maintenance_step = 7;
    }

    pub(super) fn begin_native_audio_bootstrap(&mut self) {
        let preferred = self.audio_output_device_name.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        self.audio_bootstrap_rx = Some(rx);
        let spawn = std::thread::Builder::new()
            .name("neowaves-audio-bootstrap".to_string())
            .spawn(move || {
                let result = if let Some(requested) = preferred {
                    match crate::audio::AudioEngine::new_with_output_device_name(Some(&requested)) {
                        Ok(engine) => Ok((engine, Some(requested), None)),
                        Err(preferred_error) => match crate::audio::AudioEngine::new() {
                            Ok(engine) => Ok((
                                engine,
                                None,
                                Some(format!(
                                    "Preferred output device could not be opened; using the default device: {preferred_error}"
                                )),
                            )),
                            Err(default_error) => Err(format!(
                                "Audio initialization failed. Preferred device: {preferred_error}; default device: {default_error}"
                            )),
                        },
                    }
                } else {
                    crate::audio::AudioEngine::new()
                        .map(|engine| (engine, None, None))
                        .map_err(|error| format!("Audio initialization failed: {error}"))
                };
                let _ = tx.send(result);
            });
        if let Err(error) = spawn {
            self.audio_bootstrap_rx = None;
            self.audio_output_error = Some(format!("Audio initialization thread failed: {error}"));
            self.finish_native_startup_paths();
        }
    }

    fn finish_native_startup_paths(&mut self) {
        if self.startup_paths_applied {
            return;
        }
        self.apply_startup_paths();
        self.setup_debug_automation();
        self.startup_paths_applied = true;
        self.startup_maintenance_next_at =
            std::time::Instant::now() + std::time::Duration::from_millis(250);
    }

    pub(super) fn tick_deferred_startup(
        &mut self,
        ctx: &egui::Context,
        now: std::time::Instant,
        had_ui_input: bool,
    ) {
        if let Some(rx) = self.audio_bootstrap_rx.take() {
            match rx.try_recv() {
                Ok(Ok((engine, selected_device, warning))) => {
                    let device_label = engine.output_device_name().map(str::to_string);
                    self.audio = engine;
                    self.audio_output_device_name = selected_device;
                    self.audio_output_error = warning;
                    self.audio_output_devices = device_label.into_iter().collect();
                    self.sync_after_audio_engine_replaced();
                    self.finish_native_startup_paths();
                }
                Ok(Err(error)) => {
                    self.audio_output_error = Some(error);
                    self.finish_native_startup_paths();
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    self.audio_bootstrap_rx = Some(rx);
                    ctx.request_repaint_after(std::time::Duration::from_millis(10));
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.audio_output_error =
                        Some("Audio initialization thread disconnected.".to_string());
                    self.finish_native_startup_paths();
                }
            }
        }

        if !self.startup_paths_applied
            || self.startup_maintenance_step >= 7
            || now < self.startup_maintenance_next_at
        {
            return;
        }
        if had_ui_input
            || self.scan_in_progress
            || self.playback_is_playing_now()
            || self.playback_session.is_playing
        {
            self.startup_maintenance_next_at = now + std::time::Duration::from_millis(150);
            return;
        }
        match self.startup_maintenance_step {
            0 => self.cleanup_neowaves_temp_cache_files(),
            1 => self.refresh_crash_reports_on_startup(),
            2 => self.refresh_transcript_ai_status(),
            3 => self.refresh_music_ai_status(),
            4 => self.reload_zoo_gif_frames(),
            5 => self.load_tools_config(),
            6 => {
                self.load_effect_graph_library();
                self.revalidate_effect_graph_draft();
            }
            _ => {}
        }
        self.startup_maintenance_step = self.startup_maintenance_step.saturating_add(1);
        self.startup_maintenance_next_at =
            std::time::Instant::now() + std::time::Duration::from_millis(75);
        if self.startup_maintenance_step < 7 {
            ctx.request_repaint_after(std::time::Duration::from_millis(75));
        }
    }
}
