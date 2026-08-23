use std::path::{Path, PathBuf};

use super::types::{
    ColumnId, FileMeta, MediaId, MediaItem, MediaSource, SampleValueKind, SortDir, SortKey,
    Transcript, TranscriptDocument,
};
use super::WavesPreviewer;

/// Verdict for the QA list columns. `Pass` renders an empty cell — only
/// problems are worth the reader's attention.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum QaStatus {
    /// Full-decode metadata hasn't landed yet, or was measured with a setting
    /// that has since changed.
    Unknown,
    Pass,
    Fail(String),
}

impl QaStatus {
    /// Sort weight: NG above OK above not-yet-known.
    pub(super) fn sort_rank(&self) -> Option<f64> {
        match self {
            QaStatus::Unknown => None,
            QaStatus::Pass => Some(0.0),
            QaStatus::Fail(_) => Some(1.0),
        }
    }
}

impl WavesPreviewer {
    pub(super) fn is_dotfile_path(path: &Path) -> bool {
        path.file_name()
            .and_then(|s| s.to_str())
            .map(|s| s.starts_with('.'))
            .unwrap_or(false)
    }

    pub(super) fn is_internal_temp_cache_path(path: &Path) -> bool {
        super::temp_audio_ops::is_neowaves_internal_temp_path(path)
    }

    pub(super) fn is_decode_failed_path(&self, path: &Path) -> bool {
        self.meta_for_path(path)
            .and_then(|m| m.decode_error.as_ref())
            .is_some()
    }

    /// Whether the list should treat `path` as a real file.
    ///
    /// Never stats: on a network share one `stat` against an unresponsive
    /// server blocks for the SMB timeout, and one of those on the UI thread
    /// is a hung window. The answer comes from `path_status`, whose worker
    /// takes the syscall; a path not probed yet reads as present so the list
    /// does not flash grey while a share is being walked.
    pub(super) fn path_is_file_cached(&mut self, path: &Path) -> bool {
        self.path_status.status(path).treat_as_present()
    }

    /// Drop a single cached existence entry (call after deleting/renaming a file).
    pub(super) fn invalidate_fs_exists_cache_for_path(&mut self, path: &Path) {
        self.path_status.invalidate(path);
    }

    pub(super) fn item_for_id(&self, id: MediaId) -> Option<&MediaItem> {
        self.item_index
            .get(&id)
            .and_then(|&idx| self.items.get(idx))
    }

    pub(super) fn item_for_id_mut(&mut self, id: MediaId) -> Option<&mut MediaItem> {
        let idx = *self.item_index.get(&id)?;
        self.items.get_mut(idx)
    }

    pub(super) fn item_for_row(&self, row_idx: usize) -> Option<&MediaItem> {
        let id = *self.files.get(row_idx)?;
        self.item_for_id(id)
    }

    pub(super) fn item_for_path(&self, path: &Path) -> Option<&MediaItem> {
        let id = self.path_index.get(path)?;
        self.item_for_id(id)
    }

    pub(super) fn item_for_path_mut(&mut self, path: &Path) -> Option<&mut MediaItem> {
        let id = self.path_index.get(path)?;
        self.item_for_id_mut(id)
    }

    pub(super) fn is_virtual_path(&self, path: &Path) -> bool {
        self.item_for_path(path)
            .map(|item| matches!(item.source, MediaSource::Virtual | MediaSource::External))
            .unwrap_or(false)
    }

    pub(super) fn is_external_path(&self, path: &Path) -> bool {
        self.item_for_path(path)
            .map(|item| item.source == MediaSource::External)
            .unwrap_or(false)
    }

    pub(super) fn effective_display_meta_for_path(&self, path: &Path) -> Option<&FileMeta> {
        self.edited_cache
            .get(path)
            .and_then(|cached| cached.display_meta.as_ref())
            .or_else(|| {
                self.item_for_path(path)
                    .and_then(|item| item.meta.as_deref())
            })
    }

    pub(super) fn meta_for_path(&self, path: &Path) -> Option<&FileMeta> {
        self.effective_display_meta_for_path(path)
    }

    /// Whether the Length column renders `h:mm:ss` for every row. See
    /// `list_max_duration_secs` for why this only ever latches on.
    pub(super) fn list_length_uses_hours(&self) -> bool {
        self.list_max_duration_secs.round() >= 3600.0
    }

    /// Edge Zero: does the file start and end on (near) silence? The epsilon
    /// is applied here rather than at decode time so changing it in Settings
    /// re-evaluates every row without a re-decode.
    pub(super) fn qa_edge_zero_for_path(&self, path: &Path) -> QaStatus {
        let Some(edge) = self.meta_for_path(path).and_then(|m| m.edge_abs) else {
            return QaStatus::Unknown;
        };
        let eps = self.zero_cross_epsilon.max(0.0);
        let head_bad = edge.first_abs > eps;
        let tail_bad = edge.last_abs > eps;
        if !head_bad && !tail_bad {
            return QaStatus::Pass;
        }
        let mut parts = Vec::new();
        if head_bad {
            parts.push(format!("first sample {:.5}", edge.first_abs));
        }
        if tail_bad {
            parts.push(format!("last sample {:.5}", edge.last_abs));
        }
        QaStatus::Fail(format!(
            "{} (tolerance {:.5}) — may click on playback",
            parts.join(", "),
            eps
        ))
    }

    /// Over 0 dBFS. Includes the pending gain so this agrees with the Peak
    /// column right next to it, and reports unknown rather than guessing off
    /// the header pass's short-prefix estimate.
    pub(super) fn qa_over_peak_for_path(&self, path: &Path) -> QaStatus {
        let Some((peak_db, estimate)) = self
            .meta_for_path(path)
            .map(|m| (m.peak_db, m.peak_db_estimate))
        else {
            return QaStatus::Unknown;
        };
        let Some(peak_db) = peak_db.filter(|v| !v.is_nan()) else {
            return QaStatus::Unknown;
        };
        if estimate {
            return QaStatus::Unknown;
        }
        let gain_db = self.pending_gain_db_for_path(path);
        let adjusted = peak_db + gain_db;
        if !(adjusted > 0.0) {
            return QaStatus::Pass;
        }
        let mut msg = format!("Peak {adjusted:+.2} dBFS exceeds 0 dBFS");
        if gain_db.abs() > 0.0001 {
            msg.push_str(&format!(" (includes pending gain {gain_db:+.1} dB)"));
        }
        QaStatus::Fail(msg)
    }

    /// Blank Pad. A measurement taken at a threshold other than the current
    /// setting counts as unknown, which is what makes the row re-queue itself.
    pub(super) fn qa_blank_pad_for_path(&self, path: &Path) -> QaStatus {
        let Some(scan) = self
            .meta_for_path(path)
            .and_then(|m| m.blank_pad)
            .filter(|s| s.threshold_dbfs == self.blank_threshold_dbfs)
        else {
            return QaStatus::Unknown;
        };
        let threshold = scan.threshold_dbfs;
        if scan.all_silent {
            return QaStatus::Fail(format!("Entire file is below {threshold:.1} dBFS"));
        }
        let min_ms = self.blank_min_ms.max(0.0);
        let head_bad = scan.lead_ms >= min_ms && scan.lead_ms > 0.0;
        let tail_bad = scan.tail_ms >= min_ms && scan.tail_ms > 0.0;
        if !head_bad && !tail_bad {
            return QaStatus::Pass;
        }
        let mut parts = Vec::new();
        if head_bad {
            parts.push(format!("leading {:.0} ms", scan.lead_ms));
        }
        if tail_bad {
            parts.push(format!("trailing {:.0} ms", scan.tail_ms));
        }
        QaStatus::Fail(format!(
            "{} below {:.1} dBFS (limit {:.0} ms)",
            parts.join(", "),
            threshold,
            min_ms
        ))
    }

    pub(super) fn qa_status_for_column(&self, col: ColumnId, path: &Path) -> QaStatus {
        match col {
            ColumnId::EdgeZero => self.qa_edge_zero_for_path(path),
            ColumnId::OverPeak => self.qa_over_peak_for_path(path),
            ColumnId::BlankPad => self.qa_blank_pad_for_path(path),
            _ => QaStatus::Unknown,
        }
    }

    pub(super) fn effective_sample_rate_for_path(&self, path: &Path) -> Option<u32> {
        self.sample_rate_override
            .get(path)
            .copied()
            .or_else(|| self.meta_for_path(path).map(|m| m.sample_rate))
            .filter(|v| *v > 0)
    }

    pub(super) fn effective_bits_for_path(&self, path: &Path) -> Option<u16> {
        self.bit_depth_override
            .get(path)
            .copied()
            .map(|v| v.bits_per_sample())
            .or_else(|| self.meta_for_path(path).map(|m| m.bits_per_sample))
            .filter(|v| *v > 0)
    }

    pub(super) fn effective_bits_label_for_path(&self, path: &Path) -> Option<String> {
        let override_depth = self.bit_depth_override.get(path).copied();
        let bits = override_depth
            .map(|v| v.bits_per_sample())
            .or_else(|| self.meta_for_path(path).map(|m| m.bits_per_sample))
            .filter(|v| *v > 0)?;
        let kind = if let Some(depth) = override_depth {
            match depth {
                crate::wave::WavBitDepth::Float32 => SampleValueKind::Float,
                crate::wave::WavBitDepth::Pcm16 | crate::wave::WavBitDepth::Pcm24 => {
                    SampleValueKind::Int
                }
            }
        } else {
            self.meta_for_path(path)
                .map(|m| m.sample_value_kind)
                .unwrap_or(SampleValueKind::Unknown)
        };
        if bits == 32 {
            let label = match kind {
                SampleValueKind::Float => "32f",
                SampleValueKind::Int => "32i",
                SampleValueKind::Unknown => "32",
            };
            return Some(label.to_string());
        }
        Some(bits.to_string())
    }

    pub(super) fn effective_format_override_for_path(&self, path: &Path) -> Option<&str> {
        self.format_override.get(path).map(|v| v.as_str())
    }

    pub(super) fn display_name_for_path_with_format_override(
        path: &Path,
        format_override: Option<&str>,
    ) -> String {
        let mut base = PathBuf::from(Self::display_name_for_path(path));
        if let Some(ext) = format_override {
            let ext = ext.trim().trim_start_matches('.');
            if !ext.is_empty() {
                base.set_extension(ext);
            }
        }
        base.file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("(invalid)")
            .to_string()
    }

    pub(super) fn refresh_display_name_for_path(&mut self, path: &Path) {
        let format_override = self.effective_format_override_for_path(path);
        let display = Self::display_name_for_path_with_format_override(path, format_override);
        if let Some(item) = self.item_for_path_mut(path) {
            item.display_name = display.clone();
        }
        for tab in self.tabs.iter_mut() {
            if tab.path.as_path() == path {
                tab.display_name = display.clone();
            }
        }
    }

    pub(super) fn set_meta_for_path(&mut self, path: &Path, meta: FileMeta) -> bool {
        let bpm_hint = meta.bpm.filter(|v| v.is_finite() && *v > 0.0);
        let sr_hint = (meta.sample_rate > 0).then_some(meta.sample_rate);
        let audio_track_absent = meta.audio_track_absent;
        let silent_frames = meta
            .duration_secs
            .filter(|secs| secs.is_finite() && *secs > 0.0)
            .map(|secs| {
                (secs * self.audio.shared.out_sample_rate.max(1) as f32)
                    .round()
                    .max(1.0) as usize
            })
            .unwrap_or(0);
        // Single choke point for both the header and full metadata stages, so
        // the Length format latch updates in O(1) instead of a per-frame scan.
        if let Some(d) = meta.duration_secs.filter(|v| v.is_finite() && *v > 0.0) {
            self.list_max_duration_secs = self.list_max_duration_secs.max(d);
        }
        let updated = if let Some(item) = self.item_for_path_mut(path) {
            item.meta = Some(Box::new(meta));
            if let Some(sr) = sr_hint {
                self.sample_rate_probe_cache.insert(path.to_path_buf(), sr);
            }
            if let Some(bpm) = bpm_hint {
                for tab in self.tabs.iter_mut() {
                    if tab.path == path && !tab.bpm_user_set {
                        tab.bpm_value = bpm;
                    }
                }
            }
            true
        } else {
            false
        };
        if updated {
            self.list_art_textures.remove(path);
        }
        if audio_track_absent {
            let out_sr = self.audio.shared.out_sample_rate.max(1);
            for tab in self.tabs.iter_mut().filter(|tab| tab.path == path) {
                tab.audio_track_absent = true;
                tab.loading = false;
                tab.paged_asset = false;
                tab.buffer_sample_rate = out_sr;
                tab.samples_len = silent_frames;
                tab.samples_len_visual = silent_frames;
                tab.ch_samples.clear();
                tab.ch_samples_arc = std::sync::Arc::new(Vec::new());
                tab.loading_waveform_minmax.clear();
                Self::invalidate_editor_viewport_cache(tab);
            }
            if self
                .editor_decode_state
                .as_ref()
                .is_some_and(|state| state.path == path)
            {
                self.cancel_editor_decode();
            }
        }
        updated
    }

    pub(super) fn clear_meta_for_path(&mut self, path: &Path) {
        if let Some(item) = self.item_for_path_mut(path) {
            item.meta = None;
        }
        self.sample_rate_probe_cache.remove(path);
        self.list_art_textures.remove(path);
    }

    pub(super) fn transcript_for_path(&self, path: &Path) -> Option<&Transcript> {
        self.item_for_path(path)
            .and_then(|item| item.transcript.as_deref())
    }

    pub(super) fn set_transcript_for_path(
        &mut self,
        path: &Path,
        transcript: Option<Transcript>,
    ) -> bool {
        let has_transcript = transcript.is_some();
        let document = self.item_for_path(path).and_then(|item| {
            transcript.as_ref().map(|value| {
                std::sync::Arc::new(crate::app::types::TranscriptDocument::from_transcript(
                    value,
                    item.transcript_language.clone(),
                    item.audio_asset.id,
                    item.audio_asset.revision,
                ))
            })
        });
        if let Some(item) = self.item_for_path_mut(path) {
            item.transcript = transcript.map(std::sync::Arc::new);
            item.transcript_document = document;
            if item.transcript.is_none() {
                item.transcript_language = None;
            }
        } else {
            return false;
        }
        if has_transcript && !self.list_columns.transcript {
            self.list_columns.transcript = true;
        }
        true
    }

    #[cfg_attr(not(feature = "kittest"), allow(dead_code))]
    pub(super) fn transcript_language_for_path(&self, path: &Path) -> Option<&str> {
        self.item_for_path(path)
            .and_then(|item| item.transcript_language.as_deref())
    }

    pub(super) fn set_transcript_language_for_path(
        &mut self,
        path: &Path,
        language: Option<String>,
    ) -> bool {
        let normalized = language
            .map(|v| v.trim().to_ascii_lowercase())
            .filter(|v| !v.is_empty());
        let has_language = normalized.is_some();
        if let Some(item) = self.item_for_path_mut(path) {
            item.transcript_language = normalized.clone();
            if let Some(document) = item.transcript_document.as_mut() {
                std::sync::Arc::make_mut(document).language = normalized;
            }
        } else {
            return false;
        }
        if has_language && !self.list_columns.transcript_language {
            self.list_columns.transcript_language = true;
        }
        true
    }

    pub(super) fn advance_asset_revision_for_path(&mut self, path: &Path) {
        if let Some(item) = self.item_for_path_mut(path) {
            item.audio_asset.revision = item.audio_asset.revision.next();
            if let Some(document) = item.transcript_document.as_mut() {
                std::sync::Arc::make_mut(document).asset_revision = item.audio_asset.revision;
            }
        }
    }

    pub(super) fn mutate_transcript_document_for_path<F>(&mut self, path: &Path, mutate: F)
    where
        F: FnOnce(&mut TranscriptDocument),
    {
        if let Some(item) = self.item_for_path_mut(path) {
            if let Some(document) = item.transcript_document.as_mut() {
                let document = std::sync::Arc::make_mut(document);
                mutate(document);
                item.transcript = Some(std::sync::Arc::new(document.transcript()));
                item.transcript_language = document.language.clone();
            }
        }
    }

    pub(super) fn clear_transcript_for_path(&mut self, path: &Path) {
        if let Some(item) = self.item_for_path_mut(path) {
            item.transcript = None;
            item.transcript_document = None;
            item.transcript_language = None;
        }
    }

    pub(super) fn display_name_for_path(path: &Path) -> String {
        path.file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("(invalid)")
            .to_string()
    }

    pub(super) fn display_folder_for_path(path: &Path) -> String {
        path.parent()
            .and_then(|p| p.to_str())
            .unwrap_or("")
            .to_string()
    }

    pub(super) fn rebuild_item_indexes(&mut self) {
        self.item_index.clear();
        self.path_index.clear();
        for (idx, item) in self.items.iter().enumerate() {
            self.item_index.insert(item.id, idx);
            self.path_index.insert(item.path.clone(), item.id);
        }
    }

    pub(super) fn path_for_row(&self, row_idx: usize) -> Option<&PathBuf> {
        let id = *self.files.get(row_idx)?;
        self.item_for_id(id).map(|item| &item.path)
    }

    pub(super) fn row_for_path(&self, path: &Path) -> Option<usize> {
        let id = self.path_index.get(path)?;
        self.files.iter().position(|&i| i == id)
    }

    pub(super) fn selected_path_buf(&self) -> Option<PathBuf> {
        self.selected.and_then(|i| self.path_for_row(i).cloned())
    }

    pub(super) fn selected_paths(&self) -> Vec<PathBuf> {
        let mut rows: Vec<usize> = self.selected_multi.iter().copied().collect();
        if rows.is_empty() {
            if let Some(sel) = self.selected {
                rows.push(sel);
            } else if let Some(idx) = self.active_tab {
                if let Some(tab) = self.tabs.get(idx) {
                    return vec![tab.path.clone()];
                }
            }
        }
        rows.sort_unstable();
        rows.into_iter()
            .filter_map(|row| self.path_for_row(row).cloned())
            .collect()
    }

    /// Select every visible row (multi-selection over the filtered list).
    pub(super) fn list_select_all(&mut self) {
        self.selected_multi.clear();
        for row in 0..self.files.len() {
            self.selected_multi.insert(row);
        }
        if self.selected.is_none() && !self.files.is_empty() {
            self.selected = Some(0);
        }
        self.select_anchor = self.selected;
    }

    /// Drop the multi-selection (the focused row stays).
    pub(super) fn list_clear_selection(&mut self) {
        self.selected_multi.clear();
        self.select_anchor = None;
    }

    pub(super) fn selected_real_paths(&self) -> Vec<PathBuf> {
        self.selected_paths()
            .into_iter()
            .filter(|p| !self.is_virtual_path(p))
            .collect()
    }

    pub(super) fn selected_renameable_paths(&self) -> Vec<PathBuf> {
        self.selected_paths()
            .into_iter()
            .filter(|p| !self.is_external_path(p))
            .collect()
    }

    pub(super) fn selected_item_ids(&self) -> Vec<MediaId> {
        let mut rows: Vec<usize> = self.selected_multi.iter().copied().collect();
        if rows.is_empty() {
            if let Some(sel) = self.selected {
                rows.push(sel);
            } else if let Some(idx) = self.active_tab {
                if let Some(tab) = self.tabs.get(idx) {
                    if let Some(id) = self.path_index.get(&tab.path) {
                        return vec![id];
                    }
                }
            }
        }
        rows.sort_unstable();
        rows.into_iter()
            .filter_map(|row| self.files.get(row).copied())
            .collect()
    }

    pub(super) fn ensure_sort_key_visible(&mut self) {
        let cols = self.list_columns;
        let external_visible = cols.external && !self.external_visible_columns.is_empty();
        let key_visible = match self.sort_key {
            SortKey::File => cols.file,
            SortKey::Folder => cols.folder,
            SortKey::Transcript => cols.transcript,
            SortKey::Type => cols.type_badge,
            SortKey::Length => cols.length,
            SortKey::Channels => cols.channels,
            SortKey::SampleRate => cols.sample_rate,
            SortKey::Bits => cols.bits,
            SortKey::BitRate => cols.bit_rate,
            SortKey::Level => cols.peak,
            SortKey::Lufs => cols.lufs,
            SortKey::TruePeak => cols.dbtp,
            SortKey::LufsShort => cols.lufs_s,
            SortKey::LufsMomentary => cols.lufs_m,
            SortKey::Bpm => cols.bpm,
            SortKey::SilenceLead => cols.silence_lead,
            SortKey::SilenceTail => cols.silence_tail,
            SortKey::EdgeZero => cols.edge_zero,
            SortKey::OverPeak => cols.over_peak,
            SortKey::BlankPad => cols.blank_pad,
            SortKey::CreatedAt => cols.created_at,
            SortKey::ModifiedAt => cols.modified_at,
            SortKey::External(idx) => external_visible && idx < self.external_visible_columns.len(),
            SortKey::Metadata(index) => self
                .metadata_list_columns
                .get(index)
                .is_some_and(|column| column.visible),
        };
        if key_visible {
            return;
        }
        let fallback = if cols.file {
            SortKey::File
        } else if cols.folder {
            SortKey::Folder
        } else if cols.transcript {
            SortKey::Transcript
        } else if cols.type_badge {
            SortKey::Type
        } else if external_visible {
            SortKey::External(0)
        } else if cols.length {
            SortKey::Length
        } else if cols.channels {
            SortKey::Channels
        } else if cols.sample_rate {
            SortKey::SampleRate
        } else if cols.bits {
            SortKey::Bits
        } else if cols.bit_rate {
            SortKey::BitRate
        } else if cols.peak {
            SortKey::Level
        } else if cols.lufs {
            SortKey::Lufs
        } else if cols.dbtp {
            SortKey::TruePeak
        } else if cols.lufs_s {
            SortKey::LufsShort
        } else if cols.lufs_m {
            SortKey::LufsMomentary
        } else if cols.bpm {
            SortKey::Bpm
        } else if cols.created_at {
            SortKey::CreatedAt
        } else if cols.modified_at {
            SortKey::ModifiedAt
        } else if cols.edge_zero {
            SortKey::EdgeZero
        } else if cols.over_peak {
            SortKey::OverPeak
        } else if cols.blank_pad {
            SortKey::BlankPad
        } else {
            SortKey::File
        };
        self.sort_key = fallback;
        self.sort_dir = SortDir::None;
    }

    pub(super) fn request_list_autoplay(&mut self) {
        if !self.auto_play_list_nav {
            if let Some(state) = &mut self.processing {
                state.autoplay_when_ready = false;
            }
            return;
        }
        let Some(path) = self.selected_path_buf() else {
            if let Some(state) = &mut self.processing {
                state.autoplay_when_ready = false;
            }
            return;
        };
        if let Some(state) = &mut self.processing {
            if state.path == path {
                // Defer playback until heavy processing (pitch/time) finishes.
                state.autoplay_when_ready = true;
                return;
            }
            state.autoplay_when_ready = false;
        }
        if self.list_preview_rx.is_some() {
            // Keep autoplay intent while async preview is still loading.
            self.list_play_pending = true;
            self.debug.autoplay_pending_count = self.debug.autoplay_pending_count.saturating_add(1);
            return;
        }
        if self.list_preview_pending_path.is_some() {
            self.list_play_pending = true;
            self.debug.autoplay_pending_count = self.debug.autoplay_pending_count.saturating_add(1);
            return;
        }
        self.audio.play();
        self.debug_mark_list_play_start(&path);
    }

    pub(super) fn current_active_path(&self) -> Option<&PathBuf> {
        if let Some(i) = self.active_tab {
            return self.tabs.get(i).map(|t| &t.path);
        }
        if let Some(i) = self.selected {
            return self.path_for_row(i);
        }
        None
    }
}
