use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::audio::AudioBuffer;
use crate::ipc;

use super::external_ops;
use super::session_sync;
use super::project::{
    can_store_relative, describe_missing, deserialize_project,
    fade_shape_from_str, load_sidecar_audio, loop_mode_from_str, loop_shape_from_str,
    marker_entry_to_project, metadata_sub_view_from_project, missing_file_meta,
    primary_view_from_project, project_channel_view_to_channel_view, project_marker_to_entry,
    project_music_analysis_to_draft, project_plugin_fx_chain_from_draft,
    project_plugin_fx_chain_to_draft, project_plugin_fx_draft_from_draft,
    project_plugin_fx_draft_to_draft, project_region_to_entry, project_spectrogram_from_cfg,
    project_tab_from_tab, project_tool_state_to_tool_state, region_entry_to_project, rel_path,
    repair_project_source_paths, resolve_path, serialize_project, session_path,
    spectro_config_from_project, tool_kind_from_str, ProjectApp, ProjectAppliedEffectGraph,
    ProjectAsset, ProjectBitDepthOverride, ProjectEdit, ProjectEffectGraphUi, ProjectExportPolicy,
    ProjectExternalSource, ProjectExternalState, ProjectFile, ProjectFormatOverride, ProjectList,
    ProjectListColumns, ProjectListItem, ProjectSampleRateOverride, ProjectToolState,
    ProjectTranscriptDocument, ProjectTranscriptLanguage, ProjectVirtualItem, ProjectVirtualOp,
    ProjectVirtualSource, SessionPathMode, SessionPathRepair,
};
use super::types::{LoopXfadeShape, MediaSource, VirtualOp, VirtualSourceRef, VirtualState};

/// A session document that has been read, version-checked and had its
/// source paths repaired -- everything that can be done without touching
/// app state, and therefore everything that can run off the UI thread.
pub(super) struct ParsedSession {
    pub path: PathBuf,
    /// Boxed: `ProjectFile` is large and this crosses a channel.
    pub project: Box<ProjectFile>,
    /// The exact bytes this document was read from. A later save refuses to
    /// commit unless the file still matches, so another person's save in the
    /// meantime is reported rather than overwritten.
    pub fingerprint: session_sync::SessionFingerprint,
    /// The same document hashed without its conversation or its save stamp,
    /// so a later change on disk can be classified as "somebody commented"
    /// rather than "somebody saved". Computed here because the alternative
    /// is a full re-serialize on the UI thread.
    pub comment_free_fingerprint: session_sync::SessionFingerprint,
    pub path_repair: SessionPathRepair,
    pub session_path_mode: SessionPathMode,
    pub base_dir: PathBuf,
    /// Whether each entry of `project.list.files` exists on disk, in the
    /// same order. Computed on the worker: the UI thread used to stat every
    /// path itself while building the list, which on a large session or a
    /// network share is the single longest blocking step of an open.
    pub file_exists: Vec<bool>,
    /// Existence of every *other* path the restore has to check — tab
    /// sources, virtual sources, managed assets, external data sources.
    /// The apply stage runs on the UI thread, so it must look answers up
    /// here rather than take a syscall that could block on an SMB timeout.
    ///
    /// A path absent from the map counts as present: the restore then takes
    /// its normal route and whatever it tries next reports its own failure,
    /// which is what happened before the map existed.
    pub other_exists: rustc_hash::FxHashMap<PathBuf, bool>,
}

/// Audio a session restore needs, decoded ahead of the apply stage.
///
/// The apply stage used to call `load_sidecar_audio` / `decode_audio_multi`
/// inline, once per virtual item, cached edit, tab and preview overlay, all
/// inside the frame that opened the session. Decoding is the dominant cost
/// of restoring a session with edits, so it happens on workers first and
/// the apply stage only looks results up here.
#[derive(Default)]
pub(super) struct SessionAudioPrefetch {
    /// Keyed by the document's raw sidecar reference, as written.
    sidecars: std::collections::HashMap<String, (Vec<Vec<f32>>, u32)>,
    /// Sidecars already normalized and cached for the editor.
    prepared: std::collections::HashMap<String, PreparedSidecar>,
    /// Keyed by the resolved source path.
    files: std::collections::HashMap<PathBuf, (Vec<Vec<f32>>, u32)>,
}

impl SessionAudioPrefetch {
    /// Take the decoded sidecar, if it decoded.
    ///
    /// Taking (rather than cloning) keeps one copy of each buffer in
    /// memory, which matters for a session full of long edits. A miss --
    /// the decode failed, the reference was not collected, or a second
    /// call for a reference two parts of the document share -- puts the
    /// caller back on the inline decode it always had.
    pub(super) fn take_sidecar(&mut self, raw: &str) -> Option<(Vec<Vec<f32>>, u32)> {
        self.sidecars.remove(raw)
    }

    pub(super) fn take_file(&mut self, path: &Path) -> Option<(Vec<Vec<f32>>, u32)> {
        self.files.remove(path)
    }

    /// Take a sidecar the worker already normalized and cached. Same
    /// take-not-clone rule as `take_sidecar`.
    pub(super) fn take_prepared(&mut self, raw: &str) -> Option<PreparedSidecar> {
        self.prepared.remove(raw)
    }
}

/// A sidecar that the restore turns into an editor buffer. Normalizing it
/// to the output rate and building its waveform pyramid are both O(n) passes
/// over the whole clip; done on the UI thread, once per edited tab, they are
/// what is left of a frozen session open after the decode moved to workers.
#[derive(Clone, Copy)]
struct SidecarPrep {
    /// `buffer_sample_rate` as stored in the document, if it has one.
    stored_buffer_sr: Option<u32>,
    out_sr: u32,
    quality: crate::wave::ResampleQuality,
}

/// A sidecar decoded, resampled and turned into editor caches, ready for the
/// apply stage to move into place without touching the samples again.
pub(super) struct PreparedSidecar {
    pub channels: Vec<Vec<f32>>,
    pub buffer_sample_rate: u32,
    pub samples_len: usize,
    pub waveform_minmax: Vec<(f32, f32)>,
    pub waveform_pyramid:
        Option<std::sync::Arc<crate::app::render::waveform_pyramid::WaveformPyramidSet>>,
}

/// One thing to decode before the apply stage runs.
enum PrefetchRequest {
    Sidecar {
        raw: String,
        /// Set for sidecars the restore turns into editor buffers; the
        /// worker then does the resample and cache build too.
        prep: Option<SidecarPrep>,
    },
    File {
        path: PathBuf,
    },
}

enum PrefetchResult {
    Sidecar {
        raw: String,
        channels: Vec<Vec<f32>>,
        sample_rate: u32,
    },
    Prepared {
        raw: String,
        prepared: Box<PreparedSidecar>,
    },
    File {
        path: PathBuf,
        channels: Vec<Vec<f32>>,
        sample_rate: u32,
    },
}

/// A step of the decode stage, reported as it happens so a long load shows
/// movement instead of a bare elapsed counter.
pub(super) enum PrefetchProgress {
    Started { label: String },
    Finished { label: String },
}

/// What the topbar shows about a session open in progress.
#[derive(Default)]
pub(super) struct SessionOpenProgress {
    pub done: usize,
    pub total: usize,
    /// Items a worker has started but not finished, oldest first. The first
    /// entry is what a stalled load is stuck on.
    pub in_flight: Vec<String>,
    /// When `done` last changed. A load that stops moving for long enough is
    /// reported as waiting on the server rather than left looking hung.
    pub last_progress_at: Option<Instant>,
}

impl SessionOpenProgress {
    /// Nothing has completed for this long -> say what we are waiting on.
    const STALL_AFTER: std::time::Duration = std::time::Duration::from_secs(10);

    pub fn fraction(&self) -> Option<f32> {
        (self.total > 0).then(|| (self.done as f32 / self.total as f32).clamp(0.0, 1.0))
    }

    pub fn stalled_on(&self) -> Option<&str> {
        let since = self.last_progress_at?;
        if since.elapsed() < Self::STALL_AFTER {
            return None;
        }
        self.in_flight.first().map(String::as_str)
    }
}

/// Which part of opening a session is currently running.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum SessionOpenPhase {
    /// Waiting for the first frame to paint, so the user sees the status
    /// line before anything expensive starts.
    Announced,
    /// A worker is reading and repairing the document.
    Parsing,
    /// Workers are decoding the session's edited audio and virtual sources.
    Decoding,
    /// The parsed document is being applied to app state.
    Applying,
}

impl SessionOpenPhase {
    pub fn label(self) -> &'static str {
        match self {
            SessionOpenPhase::Announced | SessionOpenPhase::Parsing => "Reading session",
            SessionOpenPhase::Decoding => "Decoding session audio",
            SessionOpenPhase::Applying => "Restoring session",
        }
    }
}

pub(super) struct ProjectOpenState {
    pub started_at: Instant,
    pub shown: bool,
    pub phase: SessionOpenPhase,
    /// Result channel for the parse worker.
    pub parse_rx: Option<std::sync::mpsc::Receiver<Result<ParsedSession, String>>>,
    /// Result channel for the decode stage, which returns the parsed
    /// document alongside the audio it decoded.
    pub decode_rx: Option<std::sync::mpsc::Receiver<(ParsedSession, SessionAudioPrefetch)>>,
    /// Per-item progress from the decode workers.
    pub progress_rx: Option<std::sync::mpsc::Receiver<PrefetchProgress>>,
    pub progress: SessionOpenProgress,
    /// Bumped per open; a worker whose generation no longer matches has
    /// been superseded (the user opened another session) and its result is
    /// dropped rather than applied over the newer one.
    pub generation: u64,
    pub cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

fn external_key_rule_to_project(rule: super::types::ExternalKeyRule) -> &'static str {
    match rule {
        super::types::ExternalKeyRule::FileName => "file",
        super::types::ExternalKeyRule::Stem => "stem",
        super::types::ExternalKeyRule::Regex => "regex",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    /// The staged open must reach exactly the state the one-shot open
    /// reaches. `open_project_file` is the one-shot path (CLI, kittest);
    /// the GUI runs the same parse on a worker and applies the result, so
    /// splitting the two must not change what lands in app state.
    #[test]
    fn staged_open_matches_the_one_shot_open() {
        let dir = temp_dir("staged_parity");
        let audio = dir.join("tone.wav");
        crate::wave::export_channels_audio(&[vec![0.1, -0.1, 0.2]], 48_000, &audio)
            .expect("write fixture");
        let session = dir.join("parity.nwsess");
        {
            let mut app =
                crate::app::WavesPreviewer::new_headless(Default::default()).expect("headless app");
            app.replace_with_files(&[audio.clone()]);
            app.save_project_as_blocking(session.clone())
                .expect("save session");
        }

        let mut one_shot =
            crate::app::WavesPreviewer::new_headless(Default::default()).expect("headless app");
        one_shot
            .open_project_file(session.clone())
            .expect("one-shot open");

        // The staged path: parse off-thread, then apply.
        let parsed = crate::app::WavesPreviewer::parse_session_document(session.clone())
            .expect("parse session");
        let mut staged =
            crate::app::WavesPreviewer::new_headless(Default::default()).expect("headless app");
        staged.apply_parsed_session(parsed).expect("staged apply");

        let paths_of = |app: &crate::app::WavesPreviewer| -> Vec<PathBuf> {
            app.items.iter().map(|item| item.path.clone()).collect()
        };
        assert_eq!(paths_of(&one_shot), paths_of(&staged));
        assert_eq!(one_shot.files.len(), staged.files.len());
        assert_eq!(one_shot.tabs.len(), staged.tabs.len());
        assert_eq!(one_shot.project_path, staged.project_path);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A file listed by the session but gone from disk must still be marked
    /// missing. The stat moved to the parse worker, so the flag now travels
    /// in `ParsedSession::file_exists` rather than being taken on the UI
    /// thread while building rows.
    #[test]
    fn a_missing_file_is_still_flagged_after_the_stat_moved_to_the_worker() {
        let dir = temp_dir("missing_flag");
        let present = dir.join("present.wav");
        let absent = dir.join("absent.wav");
        crate::wave::export_channels_audio(&[vec![0.1, 0.2]], 48_000, &present)
            .expect("write fixture");
        let session = dir.join("missing.nwsess");
        {
            let mut app =
                crate::app::WavesPreviewer::new_headless(Default::default()).expect("headless app");
            app.replace_with_files(&[present.clone()]);
            app.save_project_as_blocking(session.clone())
                .expect("save session");
        }
        // Add a path that never existed, the way a session outlives a file.
        let text = std::fs::read_to_string(&session).expect("read session");
        let mut saved = deserialize_project(&text).expect("parse session");
        saved.list.files.push(absent.to_string_lossy().to_string());
        std::fs::write(&session, serialize_project(&saved).expect("serialize"))
            .expect("rewrite session");

        let parsed = crate::app::WavesPreviewer::parse_session_document(session.clone())
            .expect("parse session");
        assert_eq!(parsed.file_exists.len(), parsed.project.list.files.len());
        let mut app =
            crate::app::WavesPreviewer::new_headless(Default::default()).expect("headless app");
        app.apply_parsed_session(parsed).expect("apply");

        let absent_status = app
            .item_for_path(&absent)
            .map(|item| item.status.clone())
            .expect("absent row present in the list");
        assert!(
            matches!(
                absent_status,
                super::super::types::MediaStatus::DecodeFailed(_)
            ),
            "a session row whose file is gone must still read as missing, got {absent_status:?}"
        );
        let present_status = app
            .item_for_path(&present)
            .map(|item| item.status.clone())
            .expect("present row");
        assert!(matches!(
            present_status,
            super::super::types::MediaStatus::Ok
        ));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Prefetching the audio must not change the restored state: the apply
    /// stage reads decoded buffers out of the prefetch instead of decoding
    /// inline, and a session with an edited tab must come back identical
    /// either way.
    #[test]
    fn prefetched_audio_restores_the_same_session_as_inline_decoding() {
        let dir = temp_dir("prefetch_parity");
        let audio = dir.join("edit.wav");
        crate::wave::export_channels_audio(&[vec![0.3, -0.3, 0.1, 0.0]], 48_000, &audio)
            .expect("write fixture");
        let session = dir.join("prefetch.nwsess");
        {
            let mut app =
                crate::app::WavesPreviewer::new_headless(Default::default()).expect("headless app");
            app.replace_with_files(&[audio.clone()]);
            app.open_or_activate_tab(&audio);
            // A sidecar is only written for a dirty tab with samples, and a
            // session with no sidecar would exercise nothing here.
            let tab = app.tabs.first_mut().expect("tab opened");
            tab.ch_samples = vec![vec![0.5, -0.25, 0.125, -0.0625]];
            tab.ch_samples_arc = std::sync::Arc::new(tab.ch_samples.clone());
            tab.samples_len = 4;
            tab.buffer_sample_rate = 48_000;
            tab.dirty = true;
            app.save_project_as_blocking(session.clone())
                .expect("save session");
        }

        let inline_parsed = crate::app::WavesPreviewer::parse_session_document(session.clone())
            .expect("parse for inline");
        let mut inline =
            crate::app::WavesPreviewer::new_headless(Default::default()).expect("headless app");
        inline
            .apply_parsed_session(inline_parsed)
            .expect("inline apply");

        let prefetched_parsed = crate::app::WavesPreviewer::parse_session_document(session.clone())
            .expect("parse for prefetch");
        // Collecting reads the output rate and resampler quality off the
        // app, so it needs one to ask.
        let staged_probe =
            crate::app::WavesPreviewer::new_headless(Default::default()).expect("headless app");
        let requests = staged_probe.collect_prefetch_requests(&prefetched_parsed);
        assert!(
            !requests.is_empty(),
            "the fixture must produce something to prefetch, or this proves nothing"
        );
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let prefetch = crate::app::WavesPreviewer::run_audio_prefetch(
            &prefetched_parsed.path.clone(),
            requests,
            2,
            &cancel,
            None,
        );
        assert!(
            !prefetch.prepared.is_empty(),
            "the tab's edited audio should have been prepared off the UI thread"
        );
        // Prove the worker did the whole job, not just the decode: the
        // buffer is normalized and the editor's waveform caches are built,
        // so the apply stage only moves them into place.
        assert!(
            prefetch.prepared.values().any(|prep| {
                prep.channels.as_slice() == [vec![0.5f32, -0.25, 0.125, -0.0625]]
                    && prep.samples_len == 4
                    && !prep.waveform_minmax.is_empty()
                    && prep.waveform_pyramid.is_some()
            }),
            "prepared sidecar should carry the saved audio and its caches, got {:?}",
            prefetch
                .prepared
                .values()
                .map(|p| (
                    p.channels.len(),
                    p.samples_len,
                    p.waveform_minmax.len(),
                    p.waveform_pyramid.is_some()
                ))
                .collect::<Vec<_>>()
        );
        let mut staged =
            crate::app::WavesPreviewer::new_headless(Default::default()).expect("headless app");
        staged
            .apply_parsed_session_with_audio(prefetched_parsed, prefetch)
            .expect("prefetched apply");

        // Opening the tab moves the restored edit out of `edited_cache` and
        // hands the audio to the async editor decode, which a headless app
        // never pumps -- so compare the state the apply itself produced.
        assert_eq!(inline.items.len(), staged.items.len());
        assert_eq!(inline.tabs.len(), staged.tabs.len());
        assert!(!inline.tabs.is_empty(), "the fixture should restore a tab");
        for (a, b) in inline.tabs.iter().zip(staged.tabs.iter()) {
            assert_eq!(a.path, b.path);
            assert_eq!(a.dirty, b.dirty);
            assert_eq!(a.buffer_sample_rate, b.buffer_sample_rate);
            assert_eq!(a.samples_len_visual, b.samples_len_visual);
        }
        assert_eq!(
            inline.tabs[0].samples_len_visual, 4,
            "the restored tab should carry the saved edit's length"
        );
        assert!(inline.tabs[0].dirty, "the saved tab was dirty");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A cancelled prefetch must stop pulling work rather than decoding the
    /// whole queue anyway.
    #[test]
    fn a_cancelled_prefetch_stops_decoding() {
        let dir = temp_dir("prefetch_cancel");
        let session = dir.join("cancel.nwsess");
        std::fs::write(&session, "").expect("touch session");
        let requests: Vec<_> = (0..64)
            .map(|i| PrefetchRequest::File {
                path: dir.join(format!("never_{i}.wav")),
            })
            .collect();
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));

        let prefetch =
            crate::app::WavesPreviewer::run_audio_prefetch(&session, requests, 2, &cancel, None);

        assert!(
            prefetch.files.is_empty() && prefetch.sidecars.is_empty(),
            "a cancelled prefetch must not return decoded audio"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The apply stage runs on the UI thread, so it must not stat: on a
    /// share one `stat` can block for the SMB timeout. Every existence
    /// question is answered on the parse worker instead. This checks the
    /// answers actually arrive, for a tab whose file is gone — the case the
    /// apply stage used to probe itself, once per tab.
    #[test]
    fn a_missing_tab_source_is_resolved_by_the_parse_worker() {
        let dir = temp_dir("tab_probe");
        let audio = dir.join("tab.wav");
        crate::wave::export_channels_audio(&[vec![0.2, -0.2]], 48_000, &audio)
            .expect("write fixture");
        let session = dir.join("tab_probe.nwsess");
        {
            let mut app =
                crate::app::WavesPreviewer::new_headless(Default::default()).expect("headless app");
            app.replace_with_files(&[audio.clone()]);
            app.open_or_activate_tab(&audio);
            app.save_project_as_blocking(session.clone())
                .expect("save session");
        }
        // The file disappears between saving and reopening, the way a
        // session outlives what it points at.
        std::fs::remove_file(&audio).expect("remove fixture");

        let parsed = crate::app::WavesPreviewer::parse_session_document(session.clone())
            .expect("parse session");

        assert_eq!(
            parsed.other_exists.get(&audio),
            Some(&false),
            "the parse worker should have probed the tab's source"
        );
        // A path the collector does not cover is simply absent, which the
        // apply stage reads as present: it then takes its normal route and
        // reports its own failure.
        assert!(!parsed
            .other_exists
            .contains_key(&dir.join("never_referenced.wav")));

        let mut app =
            crate::app::WavesPreviewer::new_headless(Default::default()).expect("headless app");
        app.apply_parsed_session(parsed).expect("apply");
        assert!(
            app.tabs.iter().any(|tab| tab.path == audio),
            "the tab should still be restored so the user can see what is missing"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A long load must show movement, not just a rising second counter:
    /// that is the difference between "working" and "hung" to the user.
    #[test]
    fn the_prefetch_reports_progress_per_item() {
        let dir = temp_dir("progress");
        let session = dir.join("progress.nwsess");
        std::fs::write(&session, "").expect("touch session");
        // Three sidecars that will not decode. Failures still have to be
        // reported as progress, or a session full of missing edits would
        // sit at 0/3 forever.
        let requests: Vec<_> = ["a.wav", "b.wav", "c.wav"]
            .into_iter()
            .map(|name| PrefetchRequest::Sidecar {
                raw: format!("sidecars/{name}"),
                prep: None,
            })
            .collect();
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (tx, rx) = std::sync::mpsc::channel();

        crate::app::WavesPreviewer::run_audio_prefetch(&session, requests, 2, &cancel, Some(tx));

        let mut started = Vec::new();
        let mut finished = Vec::new();
        while let Ok(update) = rx.try_recv() {
            match update {
                PrefetchProgress::Started { label } => started.push(label),
                PrefetchProgress::Finished { label } => finished.push(label),
            }
        }
        started.sort();
        finished.sort();
        assert_eq!(started, vec!["a.wav", "b.wav", "c.wav"]);
        assert_eq!(finished, vec!["a.wav", "b.wav", "c.wav"]);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The label is what the user sees while a share is slow, so it has to
    /// be the file name rather than the document's internal reference.
    #[test]
    fn progress_labels_are_file_names() {
        assert_eq!(
            crate::app::WavesPreviewer::prefetch_request_label(&PrefetchRequest::Sidecar {
                raw: "sidecars/session_tab_0.wav".to_string(),
                prep: None,
            }),
            "session_tab_0.wav"
        );
        assert_eq!(
            crate::app::WavesPreviewer::prefetch_request_label(&PrefetchRequest::File {
                path: PathBuf::from("/mnt/share/kicks/kick_01.wav"),
            }),
            "kick_01.wav"
        );
    }

    /// A load that stops moving has to say so, and only after long enough
    /// that a merely slow file does not trip it.
    #[test]
    fn a_stalled_load_names_what_it_is_waiting_on() {
        let mut progress = SessionOpenProgress {
            done: 3,
            total: 10,
            in_flight: vec!["slow.wav".to_string(), "next.wav".to_string()],
            last_progress_at: Some(Instant::now()),
        };
        assert_eq!(progress.stalled_on(), None, "fresh progress is not stalled");
        assert_eq!(progress.fraction(), Some(0.3));

        progress.last_progress_at = Some(
            Instant::now() - SessionOpenProgress::STALL_AFTER - std::time::Duration::from_secs(1),
        );

        assert_eq!(
            progress.stalled_on(),
            Some("slow.wav"),
            "a stalled load should name the oldest item still in flight"
        );

        // Nothing in flight means nothing to blame, even when stalled.
        progress.in_flight.clear();
        assert_eq!(progress.stalled_on(), None);
    }

    fn temp_dir(tag: &str) -> PathBuf {
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let seq = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "neowaves_session_unit_{tag}_{}_{}_{}",
            std::process::id(),
            now_ms,
            seq
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    #[test]
    fn close_project_with_autosave_writes_existing_session_and_closes() {
        let dir = temp_dir("close_save");
        let session = dir.join("saved.nwsess");
        let mut app = crate::app::WavesPreviewer::new_headless(crate::StartupConfig::default())
            .expect("headless app");
        app.project_path = Some(session.clone());

        app.close_project_with_autosave().expect("close saves");

        assert!(session.is_file(), "session should be written before close");
        assert_eq!(app.project_path, None);
        let text = std::fs::read_to_string(&session).expect("read session");
        assert!(text.contains("version = 2"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn close_project_with_autosave_keeps_session_open_on_save_error() {
        let dir = temp_dir("close_save_error");
        let blocked = dir.join("blocked.nwsess");
        std::fs::create_dir_all(&blocked).expect("create blocked session dir");
        let mut app = crate::app::WavesPreviewer::new_headless(crate::StartupConfig::default())
            .expect("headless app");
        app.project_path = Some(blocked.clone());

        let err = app
            .close_project_with_autosave()
            .expect_err("directory session path should fail");

        assert!(!err.is_empty());
        assert_eq!(app.project_path, Some(blocked));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn absolute_session_relocates_found_files_updates_document_and_keeps_missing_rows() {
        let dir = temp_dir("absolute_relocation");
        let old_dir = dir.join("old");
        let new_dir = dir.join("moved");
        let old_audio_dir = old_dir.join("audio");
        let new_audio_dir = new_dir.join("audio");
        std::fs::create_dir_all(&old_audio_dir).expect("create original audio dir");
        std::fs::create_dir_all(&new_audio_dir).expect("create relocated audio dir");
        let old_found = old_audio_dir.join("found.wav");
        let old_missing = old_audio_dir.join("missing.wav");
        std::fs::write(&old_found, b"fixture").expect("write source fixture");
        let old_session = old_dir.join("portable.nwsess");

        {
            let mut app = crate::app::WavesPreviewer::new_headless(crate::StartupConfig::default())
                .expect("headless app");
            app.replace_with_files(&[old_found.clone(), old_missing.clone()]);
            app.session_path_mode = SessionPathMode::Absolute;
            app.save_project_as_blocking(old_session.clone())
                .expect("save absolute session");
        }
        // `replace_with_files` intentionally filters nonexistent input during
        // an interactive add. Insert a once-valid serialized source to model a
        // file that disappeared after the session was saved.
        let saved_text = std::fs::read_to_string(&old_session).expect("read original session");
        let mut saved = deserialize_project(&saved_text).expect("parse original session");
        saved
            .list
            .files
            .push(old_missing.to_string_lossy().to_string());
        std::fs::write(
            &old_session,
            serialize_project(&saved).expect("serialize missing-source session"),
        )
        .expect("write missing-source session");

        let relocated_found = new_audio_dir.join("found.wav");
        std::fs::rename(&old_found, &relocated_found).expect("move source fixture");
        let relocated_session = new_dir.join("portable.nwsess");
        std::fs::rename(&old_session, &relocated_session).expect("move session");
        let moved_text =
            std::fs::read_to_string(&relocated_session).expect("read moved session");

        let mut restored =
            crate::app::WavesPreviewer::new_headless(crate::StartupConfig::default())
                .expect("restored app");
        restored
            .open_project_file(relocated_session.clone())
            .expect("open relocated session with a missing peer");

        assert_eq!(restored.items.len(), 2, "missing rows must not be dropped");
        assert!(
            restored
                .items
                .iter()
                .any(|item| item.path == relocated_found),
            "found source should follow the moved session"
        );
        let missing = restored
            .items
            .iter()
            .find(|item| item.path == old_missing)
            .expect("unresolved source remains at its last absolute path");
        assert!(matches!(
            missing.status,
            super::super::types::MediaStatus::DecodeFailed(_)
        ));

        // Opening must not have touched the file. On a shared file server
        // the repair-on-open write made every reader a writer, racing the
        // people actually saving; the repair now rides in memory until the
        // next explicit save.
        let after_open =
            std::fs::read_to_string(&relocated_session).expect("read session after open");
        assert_eq!(
            after_open, moved_text,
            "opening a relocated session must leave the file on disk untouched"
        );
        assert!(
            restored.session_paths_repaired,
            "the pending repair has to be remembered so a later save writes it"
        );

        // The repair reaches disk on the next save, which is the same
        // compare-and-swapped write as any other.
        restored
            .save_project_as_blocking(relocated_session.clone())
            .expect("save the repaired session");
        let updated_text =
            std::fs::read_to_string(&relocated_session).expect("read self-healed session");
        let updated = deserialize_project(&updated_text).expect("parse self-healed session");
        assert_eq!(
            updated.path_mode.as_deref(),
            Some("absolute"),
            "one policy applies to the whole session"
        );
        assert!(
            updated
                .list
                .files
                .iter()
                .any(|raw| PathBuf::from(raw) == relocated_found),
            "successful fallback must update the stored absolute path"
        );
        assert!(
            updated
                .list
                .files
                .iter()
                .any(|raw| PathBuf::from(raw) == old_missing),
            "an unresolved source must remain recoverable for a later reopen"
        );
        assert!(
            updated
                .list
                .files
                .iter()
                .all(|raw| Path::new(raw).is_absolute()),
            "absolute mode must never mix per-file path policies"
        );
        assert_eq!(
            updated.base_dir.as_deref().map(PathBuf::from),
            Some(new_dir),
            "the new session root is persisted after repair"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn relative_session_serializes_every_source_with_one_policy() {
        let dir = temp_dir("relative_policy");
        let audio_dir = dir.join("audio");
        std::fs::create_dir_all(&audio_dir).expect("create audio dir");
        let first = audio_dir.join("first.wav");
        let second = audio_dir.join("second.wav");
        std::fs::write(&first, b"fixture").expect("write first fixture");
        std::fs::write(&second, b"fixture").expect("write second fixture");
        let session = dir.join("relative.nwsess");

        let mut app = crate::app::WavesPreviewer::new_headless(crate::StartupConfig::default())
            .expect("headless app");
        app.replace_with_files(&[first, second]);
        app.session_path_mode = SessionPathMode::Relative;
        app.save_project_as_blocking(session.clone())
            .expect("save relative session");

        let text = std::fs::read_to_string(&session).expect("read relative session");
        let stored = deserialize_project(&text).expect("parse relative session");
        assert_eq!(stored.path_mode.as_deref(), Some("relative"));
        assert!(
            stored
                .list
                .files
                .iter()
                .all(|raw| !Path::new(raw).is_absolute()),
            "relative mode must apply to every source"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    // ---- Shared file server: two writers, no lock ------------------------

    /// Build an app with one file listed, saved to `session`.
    fn app_with_saved_session(dir: &Path, name: &str) -> (crate::app::WavesPreviewer, PathBuf) {
        let audio = dir.join("source.wav");
        if !audio.is_file() {
            crate::wave::export_channels_audio(&[vec![0.1, -0.1, 0.2]], 48_000, &audio)
                .expect("write fixture");
        }
        let session = dir.join(name);
        let mut app = crate::app::WavesPreviewer::new_headless(crate::StartupConfig::default())
            .expect("headless app");
        app.replace_with_files(&[audio]);
        app.save_project_as_blocking(session.clone())
            .expect("initial save");
        (app, session)
    }

    #[test]
    fn a_save_stamps_the_document_so_a_conflict_can_name_who_wrote_it() {
        let dir = temp_dir("stamp");
        let (app, session) = app_with_saved_session(&dir, "stamped.nwsess");
        let stored = deserialize_project(
            &std::fs::read_to_string(&session).expect("read session"),
        )
        .expect("parse session");
        assert_eq!(stored.revision, Some(1), "the first save is revision 1");
        assert!(stored.session_id.is_some());
        assert!(stored.saved_at.is_some());
        assert!(stored.saved_by.is_some());
        assert_eq!(app.session_revision, Some(1));
        assert!(app.session_disk_fingerprint.is_some());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_second_save_advances_the_revision() {
        let dir = temp_dir("revision");
        let (mut app, session) = app_with_saved_session(&dir, "rev.nwsess");
        app.save_project_as_blocking(session.clone())
            .expect("second save");
        let stored = deserialize_project(
            &std::fs::read_to_string(&session).expect("read session"),
        )
        .expect("parse session");
        assert_eq!(stored.revision, Some(2));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_save_refuses_when_the_document_changed_since_it_was_read() {
        let dir = temp_dir("conflict");
        let (mut app, session) = app_with_saved_session(&dir, "shared.nwsess");
        let ours = std::fs::read_to_string(&session).expect("read our version");

        // Somebody else saves over it.
        let mut theirs = deserialize_project(&ours).expect("parse");
        theirs.revision = Some(42);
        theirs.saved_by = Some("tanaka".to_string());
        theirs.name = Some("their edit".to_string());
        let theirs_text = serialize_project(&theirs).expect("serialize theirs");
        std::fs::write(&session, &theirs_text).expect("their save");

        let err = app
            .save_project_as_blocking(session.clone())
            .expect_err("a save over somebody else's work must be refused");
        assert!(
            err.contains("changed on disk"),
            "the error has to say what happened, got: {err}"
        );
        assert!(err.contains("tanaka"), "and who it happened to: {err}");

        let after = std::fs::read_to_string(&session).expect("read after refusal");
        assert_eq!(
            after, theirs_text,
            "a refused save must leave their document exactly as it was"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_forced_save_wins_but_keeps_the_document_it_replaced() {
        let dir = temp_dir("force");
        let (mut app, session) = app_with_saved_session(&dir, "forced.nwsess");
        let mut theirs =
            deserialize_project(&std::fs::read_to_string(&session).expect("read")).expect("parse");
        theirs.revision = Some(9);
        theirs.name = Some("their edit".to_string());
        let theirs_text = serialize_project(&theirs).expect("serialize theirs");
        std::fs::write(&session, &theirs_text).expect("their save");

        app.save_project_as_blocking_forced(session.clone(), true)
            .expect("a forced save commits");

        let stored = deserialize_project(
            &std::fs::read_to_string(&session).expect("read session"),
        )
        .expect("parse session");
        assert_eq!(
            stored.revision,
            Some(10),
            "the revision keeps climbing from what was on disk"
        );
        let backup = crate::app::WavesPreviewer::session_backup_path(&session);
        assert_eq!(
            std::fs::read_to_string(&backup).expect("read backup"),
            theirs_text,
            "the overwritten version has to remain recoverable"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_save_to_a_new_path_is_never_a_conflict() {
        let dir = temp_dir("save_as");
        let (mut app, session) = app_with_saved_session(&dir, "original.nwsess");
        let original_id = app.session_id.clone().expect("first save issued an id");
        std::fs::write(&session, "version = 2\nrevision = 77\n").expect("someone else saves");

        let other = dir.join("copy.nwsess");
        app.save_project_as_blocking(other.clone())
            .expect("Save As to a fresh path has nothing to conflict with");
        let stored =
            deserialize_project(&std::fs::read_to_string(&other).expect("read copy"))
                .expect("parse copy");
        assert_ne!(
            stored.session_id.as_deref(),
            Some(original_id.as_str()),
            "Save As forks the document, so a later conflict names the right session"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_save_recreates_a_session_somebody_deleted_rather_than_refusing() {
        let dir = temp_dir("deleted");
        let (mut app, session) = app_with_saved_session(&dir, "gone.nwsess");
        std::fs::remove_file(&session).expect("somebody deletes it");
        app.save_project_as_blocking(session.clone())
            .expect("a deleted session is recreated, not refused");
        assert!(session.is_file());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn two_writers_editing_the_same_tab_do_not_share_a_sidecar_name() {
        // The failure this guards: both used to write `data/tab_0000.wav`,
        // so the second person's save destroyed the first person's audio
        // even when the document-level check later refused their document.
        let dir = temp_dir("sidecar_clash");
        let audio = dir.join("source.wav");
        crate::wave::export_channels_audio(&[vec![0.0; 64]], 48_000, &audio)
            .expect("write fixture");
        let session = dir.join("shared.nwsess");

        let sidecar_names = |channels: Vec<f32>| -> Vec<String> {
            let mut app =
                crate::app::WavesPreviewer::new_headless(crate::StartupConfig::default())
                    .expect("headless app");
            app.replace_with_files(&[audio.clone()]);
            app.open_or_activate_tab(&audio);
            let tab = app.tabs.first_mut().expect("tab opened");
            tab.ch_samples = vec![channels.clone()];
            tab.ch_samples_arc = std::sync::Arc::new(vec![channels]);
            tab.buffer_sample_rate = 48_000;
            tab.dirty = true;
            app.save_project_as_blocking_forced(session.clone(), true)
                .expect("save with a dirty tab");
            let stored = deserialize_project(
                &std::fs::read_to_string(&session).expect("read session"),
            )
            .expect("parse session");
            stored
                .tabs
                .iter()
                .filter_map(|tab| tab.edited_audio.clone())
                .collect()
        };

        let first = sidecar_names(vec![0.25; 64]);
        let second = sidecar_names(vec![-0.75; 64]);
        assert!(!first.is_empty(), "a dirty tab must write a sidecar");
        assert_ne!(
            first, second,
            "two different takes must not be written to the same file"
        );
        // Both sets of audio survive: nothing was overwritten.
        for name in first.iter().chain(second.iter()) {
            assert!(
                dir.join(name).is_file(),
                "both writers' audio must still be on disk: {name}"
            );
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn resaving_unchanged_audio_reuses_the_sidecar_instead_of_churning_files() {
        let dir = temp_dir("sidecar_dedup");
        let audio = dir.join("source.wav");
        crate::wave::export_channels_audio(&[vec![0.0; 64]], 48_000, &audio)
            .expect("write fixture");
        let session = dir.join("dedup.nwsess");
        let mut app = crate::app::WavesPreviewer::new_headless(crate::StartupConfig::default())
            .expect("headless app");
        app.replace_with_files(&[audio.clone()]);
        app.open_or_activate_tab(&audio);
        {
            let tab = app.tabs.first_mut().expect("tab opened");
            tab.ch_samples = vec![vec![0.5; 64]];
            tab.ch_samples_arc = std::sync::Arc::new(vec![vec![0.5; 64]]);
            tab.buffer_sample_rate = 48_000;
            tab.dirty = true;
        }
        app.save_project_as_blocking(session.clone())
            .expect("first save");
        app.save_project_as_blocking(session.clone())
            .expect("second save");

        let data_dir = super::super::project::project_data_dir(&session);
        let count = std::fs::read_dir(&data_dir)
            .expect("read data dir")
            .flatten()
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "wav"))
            .count();
        assert_eq!(
            count, 1,
            "unchanged audio must reuse its sidecar rather than pile up copies"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_committed_save_leaves_no_temp_files_behind() {
        let dir = temp_dir("no_temps");
        let (_app, session) = app_with_saved_session(&dir, "clean.nwsess");
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .expect("read dir")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .filter(|name| name.ends_with(".tmp") || name.ends_with(".stage"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "a save must clean up after itself, found {leftovers:?}"
        );
        assert!(session.is_file());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn an_empty_session_file_reports_something_a_person_can_act_on() {
        let dir = temp_dir("empty_doc");
        let session = dir.join("truncated.nwsess");
        std::fs::write(&session, "").expect("write empty session");
        let err = match crate::app::WavesPreviewer::parse_session_document(session.clone()) {
            Err(err) => err,
            Ok(_) => panic!("an empty document cannot be opened"),
        };
        assert!(
            err.contains("empty") && err.contains("interrupted"),
            "the message must explain the state, got: {err}"
        );

        // With a backup beside it, say where it is.
        let backup = crate::app::WavesPreviewer::session_backup_path(&session);
        std::fs::write(&backup, "version = 2\n").expect("write backup");
        let err = match crate::app::WavesPreviewer::parse_session_document(session) {
            Err(err) => err,
            Ok(_) => panic!("still cannot be opened"),
        };
        assert!(
            err.contains(&backup.display().to_string()),
            "the message must point at the backup, got: {err}"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn opening_does_not_write_to_the_session_file() {
        // Every reader used to be a writer whenever a path repair fired.
        let dir = temp_dir("readonly_open");
        let (_app, session) = app_with_saved_session(&dir, "readonly.nwsess");
        let before = std::fs::read(&session).expect("read before");
        let mut reader = crate::app::WavesPreviewer::new_headless(crate::StartupConfig::default())
            .expect("headless app");
        reader
            .open_project_file(session.clone())
            .expect("open session");
        let after = std::fs::read(&session).expect("read after");
        assert_eq!(before, after, "opening a session must not modify it");
        assert_eq!(
            reader.session_disk_fingerprint,
            Some(super::session_sync::SessionFingerprint::of_bytes(&after)),
            "the opener has to remember exactly what it read"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_stale_temp_sweep_only_touches_files_it_made_itself() {
        // The session's folder belongs to the user. Somebody's unrelated
        // `.tmp` in there -- an Office lock file, a colleague's scratch --
        // is not ours to delete no matter how old it is.
        let dir = temp_dir("sweep");
        let (mut app, session) = app_with_saved_session(&dir, "swept.nwsess");

        let ours = dir.join(format!(
            "swept.nwsess.{}1234.99.abcdef01.tmp",
            crate::app::WavesPreviewer::SAVE_TEMP_MARKER
        ));
        let theirs = dir.join("~$somebody-elses-document.tmp");
        let also_theirs = dir.join("render.stage");
        for path in [&ours, &theirs, &also_theirs] {
            std::fs::write(path, b"leftover").expect("write leftover");
        }
        // Age them past the sweep's grace period.
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(48 * 60 * 60);
        for path in [&ours, &theirs, &also_theirs] {
            let file = std::fs::File::options()
                .write(true)
                .open(path)
                .expect("open leftover");
            let _ = file.set_modified(old);
        }

        app.save_project_as_blocking(session)
            .expect("save runs the sweep");

        assert!(!ours.is_file(), "our own stale temp must be cleaned up");
        assert!(
            theirs.is_file(),
            "somebody else's .tmp must be left completely alone"
        );
        assert!(
            also_theirs.is_file(),
            "a .stage file we did not write is not ours to delete"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_recent_temp_of_ours_survives_the_sweep() {
        // A save in flight on another machine has stage files on disk right
        // now. Deleting those would break it.
        let dir = temp_dir("sweep_recent");
        let (mut app, session) = app_with_saved_session(&dir, "recent.nwsess");
        let in_flight = dir.join(format!(
            "other.nwsess.{}999.1.deadbeef.stage",
            crate::app::WavesPreviewer::SAVE_TEMP_MARKER
        ));
        std::fs::write(&in_flight, b"in flight").expect("write in-flight stage");

        app.save_project_as_blocking(session)
            .expect("save runs the sweep");

        assert!(
            in_flight.is_file(),
            "a temp young enough to belong to a save in flight must survive"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_legacy_sidecar_name_still_resolves_after_the_scheme_changed() {
        // Sessions written before content addressing point at `tab_0000.wav`
        // and `assets/<id>/1.wav`; nothing resolves a sidecar except through
        // the string in the document, so they must keep opening.
        let dir = temp_dir("legacy_sidecar");
        let session = dir.join("legacy.nwsess");
        let legacy_asset = super::super::project::legacy_asset_audio_dst(
            &session,
            crate::audio_asset::AudioAssetId(0x1234),
            crate::audio_asset::AssetRevision(1),
        );
        assert!(legacy_asset.to_string_lossy().ends_with("1.wav"));
        std::fs::create_dir_all(legacy_asset.parent().expect("asset parent"))
            .expect("create legacy asset dir");
        crate::wave::export_channels_audio(&[vec![0.2; 32]], 48_000, &legacy_asset)
            .expect("write legacy asset");
        let base = session.parent().expect("session parent");
        let stored = rel_path(&legacy_asset, base);
        let (channels, sample_rate, _) =
            super::super::project::load_sidecar_audio(&session, &stored)
                .expect("a legacy sidecar reference still resolves");
        assert_eq!(sample_rate, 48_000);
        assert_eq!(channels.len(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }
}

fn external_key_rule_from_project(raw: &str) -> super::types::ExternalKeyRule {
    match raw.trim().to_ascii_lowercase().as_str() {
        "stem" => super::types::ExternalKeyRule::Stem,
        "regex" => super::types::ExternalKeyRule::Regex,
        _ => super::types::ExternalKeyRule::FileName,
    }
}

fn external_match_input_to_project(input: super::types::ExternalRegexInput) -> &'static str {
    match input {
        super::types::ExternalRegexInput::FileName => "file",
        super::types::ExternalRegexInput::Stem => "stem",
        super::types::ExternalRegexInput::Path => "path",
        super::types::ExternalRegexInput::Dir => "dir",
    }
}

fn external_match_input_from_project(raw: &str) -> super::types::ExternalRegexInput {
    match raw.trim().to_ascii_lowercase().as_str() {
        "stem" => super::types::ExternalRegexInput::Stem,
        "path" => super::types::ExternalRegexInput::Path,
        "dir" => super::types::ExternalRegexInput::Dir,
        _ => super::types::ExternalRegexInput::FileName,
    }
}

fn virtual_source_to_project(
    source: &VirtualSourceRef,
    base_dir: &Path,
    path_mode: SessionPathMode,
) -> ProjectVirtualSource {
    match source {
        VirtualSourceRef::FilePath(path) => ProjectVirtualSource {
            kind: "file".to_string(),
            path: Some(session_path(path, base_dir, path_mode)),
        },
        VirtualSourceRef::VirtualPath(path) => ProjectVirtualSource {
            kind: "virtual".to_string(),
            path: Some(path.to_string_lossy().to_string()),
        },
        VirtualSourceRef::Sidecar(tag) => ProjectVirtualSource {
            kind: "sidecar".to_string(),
            path: Some(tag.clone()),
        },
    }
}

fn virtual_source_from_project(source: &ProjectVirtualSource, base_dir: &Path) -> VirtualSourceRef {
    match source.kind.trim().to_ascii_lowercase().as_str() {
        "virtual" => source
            .path
            .as_deref()
            .map(|raw| VirtualSourceRef::VirtualPath(resolve_path(raw, base_dir)))
            .unwrap_or_else(|| VirtualSourceRef::Sidecar("missing_virtual_source".to_string())),
        "sidecar" => VirtualSourceRef::Sidecar(source.path.clone().unwrap_or_default()),
        _ => source
            .path
            .as_deref()
            .map(|raw| VirtualSourceRef::FilePath(resolve_path(raw, base_dir)))
            .unwrap_or_else(|| VirtualSourceRef::Sidecar("missing_file_source".to_string())),
    }
}

fn virtual_ops_to_project(ops: &[VirtualOp]) -> Vec<ProjectVirtualOp> {
    let mut out = Vec::with_capacity(ops.len());
    for op in ops {
        match op {
            VirtualOp::Trim { start, end } => out.push(ProjectVirtualOp {
                kind: "trim".to_string(),
                start: Some(*start),
                end: Some(*end),
            }),
        }
    }
    out
}

fn virtual_ops_from_project(ops: &[ProjectVirtualOp]) -> Vec<VirtualOp> {
    let mut out = Vec::new();
    for op in ops {
        if op.kind.trim().eq_ignore_ascii_case("trim") {
            if let (Some(start), Some(end)) = (op.start, op.end) {
                if end > start {
                    out.push(VirtualOp::Trim { start, end });
                }
            }
        }
    }
    out
}

fn apply_virtual_ops(channels: &mut [Vec<f32>], ops: &[VirtualOp]) {
    for op in ops {
        match *op {
            VirtualOp::Trim { start, end } => {
                for ch in channels.iter_mut() {
                    let len = ch.len();
                    if len == 0 {
                        continue;
                    }
                    let s = start.min(len);
                    let e = end.min(len);
                    if e <= s {
                        ch.clear();
                    } else {
                        let mut seg = ch[s..e].to_vec();
                        std::mem::swap(ch, &mut seg);
                        ch.truncate(e - s);
                    }
                }
            }
        }
    }
}

impl super::WavesPreviewer {
    const RECENT_SESSION_LIMIT: usize = 10;

    pub(super) fn normalize_recent_session_path(path: &Path) -> Option<PathBuf> {
        let is_nwsess = path
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s.eq_ignore_ascii_case("nwsess"))
            .unwrap_or(false);
        if !is_nwsess || !path.is_file() {
            return None;
        }
        std::fs::canonicalize(path)
            .ok()
            .or_else(|| Some(path.to_path_buf()))
    }

    pub(super) fn set_recent_sessions_from_prefs(&mut self, paths: Vec<PathBuf>) {
        self.recent_sessions.clear();
        for path in paths {
            let Some(path) = Self::normalize_recent_session_path(&path) else {
                continue;
            };
            if !self.recent_sessions.iter().any(|p| p == &path) {
                self.recent_sessions.push(path);
            }
            if self.recent_sessions.len() >= Self::RECENT_SESSION_LIMIT {
                break;
            }
        }
    }

    pub(super) fn insert_recent_session_path(&mut self, path: &Path) -> bool {
        let Some(path) = Self::normalize_recent_session_path(path) else {
            return false;
        };
        self.recent_sessions.retain(|existing| existing != &path);
        self.recent_sessions.insert(0, path);
        self.recent_sessions.truncate(Self::RECENT_SESSION_LIMIT);
        true
    }

    pub(super) fn recent_session_paths_for_menu(&self) -> Vec<PathBuf> {
        self.recent_sessions
            .iter()
            .filter_map(|path| Self::normalize_recent_session_path(path))
            .take(Self::RECENT_SESSION_LIMIT)
            .collect()
    }

    /// The name stamped into a saved session's `saved_by`. Prefers the
    /// `display_name=` pref so a team can use names they recognise rather
    /// than OS account names.
    pub(super) fn session_saved_by(&self) -> String {
        self.session_display_name
            .clone()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(session_sync::local_display_name)
    }

    pub(super) fn add_recent_session_path(&mut self, path: &Path) {
        if self.insert_recent_session_path(path) {
            self.save_prefs();
        }
    }

    /// Blocking close, kept for the CLI, the kittest harness and unit tests
    /// that need the file on disk when the call returns. The GUI uses
    /// `request_close_project_with_autosave`.
    #[allow(dead_code)]
    pub(super) fn close_project_with_autosave(&mut self) -> Result<(), String> {
        if let Some(path) = self.project_path.clone() {
            // Blocking: the session state is torn down right after, so the
            // snapshot must be fully persisted first.
            self.save_project_as_blocking(path)?;
        }
        self.close_project();
        Ok(())
    }

    /// Interactive close. Writing a session's sidecars means encoding a WAV
    /// per edited tab and virtual item, which blocked the UI thread for as
    /// long as that took. Save on the worker instead and tear the session
    /// down when it lands; the existing busy overlay covers the wait with a
    /// message rather than a frozen window.
    pub(super) fn request_close_project_with_autosave(&mut self) -> Result<(), String> {
        let Some(path) = self.project_path.clone() else {
            self.close_project();
            return Ok(());
        };
        self.save_project_as(path)?;
        if self.session_save_state.is_some() {
            // The worker owns it now; `drain_session_save` closes on success.
            self.close_after_session_save = true;
        } else {
            // Nothing to write (the save completed inline), so close now.
            self.close_project();
        }
        Ok(())
    }

    pub(super) fn queue_project_open(&mut self, path: PathBuf) {
        // Supersede any open still in flight: its worker result will be
        // dropped on generation mismatch rather than applied over this one.
        if let Some(previous) = self.project_open_state.as_ref() {
            previous
                .cancel
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }
        self.project_open_generation = self.project_open_generation.wrapping_add(1);
        self.project_open_pending = Some(path);
        self.project_open_state = Some(ProjectOpenState {
            started_at: Instant::now(),
            shown: false,
            phase: SessionOpenPhase::Announced,
            parse_rx: None,
            decode_rx: None,
            progress_rx: None,
            progress: SessionOpenProgress::default(),
            generation: self.project_open_generation,
            cancel: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        });
    }

    /// Hand the parsed document to a coordinator thread that decodes every
    /// sidecar and virtual source the restore needs, then returns both.
    /// Decoding is the dominant cost of restoring a session with edits, and
    /// doing it here keeps it entirely off the UI thread.
    fn begin_session_audio_prefetch(
        &mut self,
        parsed: ParsedSession,
        cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) {
        let requests = self.collect_prefetch_requests(&parsed);
        let Some(state) = self.project_open_state.as_mut() else {
            return;
        };
        if requests.is_empty() {
            // Nothing to decode: apply straight away rather than paying for
            // a thread and a frame of latency.
            state.phase = SessionOpenPhase::Applying;
            if let Err(err) = self.apply_parsed_session(parsed) {
                self.debug_log(format!("session open error: {err}"));
                self.push_toast(
                    super::types::ToastSeverity::Error,
                    format!("Session open failed: {err}"),
                );
            }
            self.project_open_state = None;
            return;
        }
        let concurrency = self.perf.restore_concurrency();
        let total = requests.len();
        let (progress_tx, progress_rx) = std::sync::mpsc::channel();
        let (tx, rx) = std::sync::mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("neowaves-session-audio".to_string())
            .spawn(move || {
                crate::app::threading::lower_current_thread_priority();
                let prefetch = Self::run_audio_prefetch(
                    &parsed.path.clone(),
                    requests,
                    concurrency,
                    &cancel,
                    Some(progress_tx),
                );
                let _ = tx.send((parsed, prefetch));
                crate::ui_wake::wake_ui();
            });
        let Some(state) = self.project_open_state.as_mut() else {
            return;
        };
        match spawned {
            Ok(_) => {
                state.decode_rx = Some(rx);
                state.progress_rx = Some(progress_rx);
                state.progress = SessionOpenProgress {
                    total,
                    last_progress_at: Some(Instant::now()),
                    ..Default::default()
                };
                state.phase = SessionOpenPhase::Decoding;
            }
            Err(err) => {
                self.debug_log(format!("session decode thread failed: {err}"));
                self.project_open_state = None;
            }
        }
    }

    /// True while a session open is in flight. Callers that mutate session
    /// state (save, export, destructive edits) refuse while this holds, so
    /// a half-restored document is never written back or edited.
    pub(super) fn session_open_in_progress(&self) -> bool {
        self.project_open_state.is_some()
    }

    /// Guard for anything that writes session state while a restore is
    /// running: toast + true when the caller must refuse. Reading the list,
    /// scrolling and playback stay available -- only writes are held back,
    /// because the document is only partly applied.
    pub(super) fn session_open_busy_toast(&mut self) -> bool {
        if !self.session_open_in_progress() {
            return false;
        }
        self.push_toast(
            super::types::ToastSeverity::Info,
            "The session is still opening — wait for it or cancel it from the topbar",
        );
        true
    }

    pub(super) fn cancel_session_open(&mut self) {
        let Some(state) = self.project_open_state.take() else {
            return;
        };
        state
            .cancel
            .store(true, std::sync::atomic::Ordering::Relaxed);
        self.project_open_pending = None;
        // A cancelled open must not leave a half-applied document behind:
        // the parse phase has not touched app state yet, but the apply
        // phase has, so clear back to an empty session either way.
        if state.phase == SessionOpenPhase::Applying {
            self.close_project();
        }
        self.push_toast(super::types::ToastSeverity::Info, "Session open cancelled");
    }

    /// Drive the staged session open. Reading and repairing the document
    /// runs on a worker (the path repair stats every referenced file);
    /// applying it happens on the UI thread once the worker lands.
    pub(super) fn tick_project_open(&mut self) {
        let Some(state) = self.project_open_state.as_mut() else {
            return;
        };
        // Paint one frame with the status line before starting work, so the
        // window is visibly alive before the first expensive step.
        if !state.shown {
            state.shown = true;
            return;
        }
        match state.phase {
            SessionOpenPhase::Announced => {
                let Some(path) = self.project_open_pending.take() else {
                    self.project_open_state = None;
                    return;
                };
                let (tx, rx) = std::sync::mpsc::channel();
                let spawned = std::thread::Builder::new()
                    .name("neowaves-session-parse".to_string())
                    .spawn(move || {
                        crate::app::threading::lower_current_thread_priority();
                        let _ = tx.send(Self::parse_session_document(path));
                        crate::ui_wake::wake_ui();
                    });
                let Some(state) = self.project_open_state.as_mut() else {
                    return;
                };
                match spawned {
                    Ok(_) => {
                        state.parse_rx = Some(rx);
                        state.phase = SessionOpenPhase::Parsing;
                    }
                    Err(err) => {
                        self.debug_log(format!("session parse thread failed: {err}"));
                        self.project_open_state = None;
                    }
                }
            }
            SessionOpenPhase::Parsing => {
                let Some(rx) = state.parse_rx.as_ref() else {
                    self.project_open_state = None;
                    return;
                };
                let generation = state.generation;
                let cancel = std::sync::Arc::clone(&state.cancel);
                match rx.try_recv() {
                    Ok(result) => {
                        state.parse_rx = None;
                        // The user may have started another open while this
                        // one was parsing; that one owns the app state now.
                        if generation != self.project_open_generation {
                            self.project_open_state = None;
                            return;
                        }
                        let parsed = match result {
                            Ok(parsed) => parsed,
                            Err(err) => {
                                self.debug_log(format!("session open error: {err}"));
                                self.push_toast(
                                    super::types::ToastSeverity::Error,
                                    format!("Session open failed: {err}"),
                                );
                                self.project_open_state = None;
                                return;
                            }
                        };
                        self.begin_session_audio_prefetch(parsed, cancel);
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => {}
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        self.debug_log("session parse worker disconnected".to_string());
                        self.project_open_state = None;
                    }
                }
            }
            SessionOpenPhase::Decoding => {
                if let Some(progress_rx) = state.progress_rx.as_ref() {
                    while let Ok(update) = progress_rx.try_recv() {
                        match update {
                            PrefetchProgress::Started { label } => {
                                state.progress.in_flight.push(label);
                            }
                            PrefetchProgress::Finished { label } => {
                                if let Some(pos) =
                                    state.progress.in_flight.iter().position(|l| *l == label)
                                {
                                    state.progress.in_flight.remove(pos);
                                }
                                state.progress.done = state.progress.done.saturating_add(1);
                                state.progress.last_progress_at = Some(Instant::now());
                            }
                        }
                    }
                }
                let Some(rx) = state.decode_rx.as_ref() else {
                    self.project_open_state = None;
                    return;
                };
                let generation = state.generation;
                match rx.try_recv() {
                    Ok((parsed, prefetch)) => {
                        state.decode_rx = None;
                        state.progress_rx = None;
                        state.phase = SessionOpenPhase::Applying;
                        if generation != self.project_open_generation {
                            self.project_open_state = None;
                            return;
                        }
                        if let Err(err) = self.apply_parsed_session_with_audio(parsed, prefetch) {
                            self.debug_log(format!("session open error: {err}"));
                            self.push_toast(
                                super::types::ToastSeverity::Error,
                                format!("Session open failed: {err}"),
                            );
                        }
                        self.project_open_state = None;
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => {}
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        self.debug_log("session decode worker disconnected".to_string());
                        self.project_open_state = None;
                    }
                }
            }
            SessionOpenPhase::Applying => {
                // Applying completes within the frame that starts it; a
                // state left here means that frame returned early.
                self.project_open_state = None;
            }
        }
    }

    pub(super) fn is_session_path(path: &Path) -> bool {
        path.extension()
            .and_then(|s| s.to_str())
            .map(|s| s.eq_ignore_ascii_case("nwsess") || s.eq_ignore_ascii_case("nwproj"))
            .unwrap_or(false)
    }

    /// Fallback for a sidecar the prefetch did not cover: the same work as
    /// `prepare_sidecar`, on this thread, plus the legacy-document warning
    /// the worker cannot log.
    fn prepare_sidecar_on_ui(
        &mut self,
        path: &Path,
        channels: Vec<Vec<f32>>,
        sidecar_sr: u32,
        stored_buffer_sr: Option<u32>,
        source_label: &str,
    ) -> PreparedSidecar {
        let (channels, buffer_sr) = self.normalize_loaded_sidecar_buffer(
            path,
            channels,
            sidecar_sr.max(1),
            stored_buffer_sr,
            source_label,
        );
        let samples_len = channels.first().map(|c| c.len()).unwrap_or(0);
        let (waveform_minmax, waveform_pyramid) =
            Self::build_editor_waveform_cache(&channels, samples_len);
        PreparedSidecar {
            channels,
            buffer_sample_rate: buffer_sr,
            samples_len,
            waveform_minmax,
            waveform_pyramid,
        }
    }

    fn normalize_loaded_sidecar_buffer(
        &mut self,
        path: &Path,
        mut channels: Vec<Vec<f32>>,
        sidecar_sr: u32,
        stored_buffer_sr: Option<u32>,
        source_label: &str,
    ) -> (Vec<Vec<f32>>, u32) {
        let out_sr = self.audio.shared.out_sample_rate.max(1);
        let mut buffer_sr = stored_buffer_sr.filter(|v| *v > 0).unwrap_or_else(|| {
            if sidecar_sr.max(1) != out_sr {
                self.debug_log(format!(
                    "legacy session sidecar missing buffer_sample_rate: {} path={} sidecar_sr={} -> assuming output_sr={}",
                    source_label,
                    path.display(),
                    sidecar_sr.max(1),
                    out_sr
                ));
                out_sr
            } else {
                sidecar_sr.max(1)
            }
        });
        if buffer_sr != out_sr {
            let quality = Self::to_wave_resample_quality(self.src_quality);
            for ch in channels.iter_mut() {
                *ch = crate::wave::resample_quality(ch, buffer_sr, out_sr, quality);
            }
            buffer_sr = out_sr;
        }
        (channels, buffer_sr)
    }

    pub(super) fn save_project(&mut self) -> Result<(), String> {
        let path = match self.project_path.clone() {
            Some(p) => p,
            None => {
                let Some(mut picked) = self.pick_project_save_dialog() else {
                    return Ok(());
                };
                let needs_ext = picked
                    .extension()
                    .and_then(|s| s.to_str())
                    .map(|s| !s.eq_ignore_ascii_case("nwsess"))
                    .unwrap_or(true);
                if needs_ext {
                    picked.set_extension("nwsess");
                }
                picked
            }
        };
        let path = if path
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s.eq_ignore_ascii_case("nwproj"))
            .unwrap_or(false)
        {
            path.with_extension("nwsess")
        } else {
            path
        };
        self.save_project_as(path)
    }

    fn path_mode_for_save(&mut self, base_dir: &Path) -> SessionPathMode {
        // A session being written to a share for the first time gets
        // relative paths. Colleagues mount the same share differently --
        // `Z:\Proj` here, `\\server\share\Proj` there -- so absolute
        // paths in a shared session resolve for whoever saved it and for
        // nobody else. Relative paths follow the `.nwsess` and are correct
        // for everyone. An existing session keeps the policy it was written
        // with; only a document that has never been saved switches.
        if self.session_path_mode != SessionPathMode::Relative
            && self.project_path.is_none()
            && crate::audio_io::is_remote_file_path(base_dir)
        {
            self.session_path_mode = SessionPathMode::Relative;
            self.debug_log(
                "new session on a network share defaults to relative paths so it resolves from any machine"
                    .to_string(),
            );
        }
        if self.session_path_mode != SessionPathMode::Relative {
            return SessionPathMode::Absolute;
        }
        let mut all_relative = self
            .items
            .iter()
            .all(|item| can_store_relative(&item.path, base_dir));
        if let Some(root) = self.root.as_ref() {
            all_relative &= can_store_relative(root, base_dir);
        }
        all_relative &= self
            .external_sources
            .iter()
            .all(|source| can_store_relative(&source.path, base_dir));
        if let Some(dest) = self.export_cfg.dest_folder.as_ref() {
            all_relative &= can_store_relative(dest, base_dir);
        }
        all_relative &= self.tabs.iter().all(|tab| {
            can_store_relative(&tab.path, base_dir)
                && tab
                    .music_analysis_draft
                    .stems_dir_override
                    .as_ref()
                    .map(|path| can_store_relative(path, base_dir))
                    .unwrap_or(true)
        });
        if all_relative {
            SessionPathMode::Relative
        } else {
            // A relative session may not silently become a mixed relative /
            // absolute document when a different-volume source is added.
            self.session_path_mode = SessionPathMode::Absolute;
            self.debug_log(
                "session path mode changed to absolute: not every source can be represented relative to the session"
                    .to_string(),
            );
            SessionPathMode::Absolute
        }
    }

    /// Everything a session save needs, gathered without touching the disk:
    /// the fully-built document plus Arc snapshots of every sidecar's audio.
    /// The heavy part (WAV encodes + TOML write) runs from
    /// [`Self::run_session_save_jobs`], on a worker for interactive saves.
    fn build_session_save_plan(
        &mut self,
        path: PathBuf,
    ) -> Result<
        (
            PathBuf,
            ProjectFile,
            Vec<crate::app::types::SessionSidecarJob>,
        ),
        String,
    > {
        use crate::app::types::{SessionSidecarJob, SessionSidecarSource, SidecarSlot};
        let path = if path
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s.eq_ignore_ascii_case("nwproj"))
            .unwrap_or(false)
        {
            path.with_extension("nwsess")
        } else {
            path
        };
        let path = if path.is_absolute() {
            path
        } else {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(path)
        };
        // Save As to a different file forks the document: the copy is a new
        // lineage, and inheriting the original's id would make a later
        // conflict describe the wrong session.
        if self.project_path.as_deref() != Some(path.as_path()) {
            self.session_id = None;
        }
        let mut sidecar_jobs: Vec<SessionSidecarJob> = Vec::new();
        let base_dir = path.parent().unwrap_or_else(|| Path::new("."));
        let path_mode = self.path_mode_for_save(base_dir);
        let list_files: Vec<PathBuf> = self.items.iter().map(|i| i.path.clone()).collect();
        let mut list_items = Vec::new();
        for item in &self.items {
            if item.pending_gain_db.abs() > 0.0001
                || !item.note.is_empty()
                || !item.editor_notes.is_empty()
                || item.status_id.is_some()
                || !item.tags().is_empty()
            {
                list_items.push(ProjectListItem {
                    path: session_path(&item.path, base_dir, path_mode),
                    pending_gain_db: item.pending_gain_db,
                    note: item.note.clone(),
                    editor_notes: item.editor_notes.clone(),
                    status: item.status_id.as_deref().map(str::to_string),
                    tags: item.tags().iter().map(|tag| tag.to_string()).collect(),
                });
            }
        }
        let mut sample_rate_overrides: Vec<ProjectSampleRateOverride> = self
            .sample_rate_override
            .iter()
            .filter_map(|(path, &sample_rate)| {
                if sample_rate > 0 {
                    Some(ProjectSampleRateOverride {
                        path: session_path(path, base_dir, path_mode),
                        sample_rate,
                    })
                } else {
                    None
                }
            })
            .collect();
        sample_rate_overrides.sort_by(|a, b| a.path.cmp(&b.path));
        let mut bit_depth_overrides: Vec<ProjectBitDepthOverride> = self
            .bit_depth_override
            .iter()
            .map(|(path, depth)| ProjectBitDepthOverride {
                path: session_path(path, base_dir, path_mode),
                bit_depth: depth.project_value().to_string(),
            })
            .collect();
        bit_depth_overrides.sort_by(|a, b| a.path.cmp(&b.path));
        let mut format_overrides: Vec<ProjectFormatOverride> = self
            .format_override
            .iter()
            .filter_map(|(path, format)| {
                let ext = format.trim().trim_start_matches('.').to_ascii_lowercase();
                if ext.is_empty() || !crate::audio_io::is_supported_extension(&ext) {
                    return None;
                }
                Some(ProjectFormatOverride {
                    path: session_path(path, base_dir, path_mode),
                    format: ext,
                })
            })
            .collect();
        format_overrides.sort_by(|a, b| a.path.cmp(&b.path));
        let mut virtual_items: Vec<ProjectVirtualItem> = Vec::new();
        let mut assets: Vec<ProjectAsset> = Vec::new();
        for item in self
            .items
            .iter()
            .filter(|item| item.source == MediaSource::Virtual)
        {
            let source = item
                .virtual_state
                .as_ref()
                .map(|state| virtual_source_to_project(&state.source, base_dir, path_mode))
                .unwrap_or(ProjectVirtualSource {
                    kind: "sidecar".to_string(),
                    path: Some("runtime".to_string()),
                });
            let op_chain = item
                .virtual_state
                .as_ref()
                .map(|state| virtual_ops_to_project(&state.op_chain))
                .unwrap_or_default();
            // Snapshot the item's *current* audio (including destructive editor
            // edits sitting in a dirty tab / edited_cache), not the possibly
            // stale `virtual_audio`. The sidecar is the authoritative copy used
            // on restore, so it must reflect what the user actually sees.
            let current_audio = self
                .edited_audio_for_path(&item.path)
                .or_else(|| item.virtual_audio.clone());
            let channels = item
                .virtual_state
                .as_ref()
                .map(|state| state.channels)
                .or_else(|| item.meta.as_ref().map(|m| m.channels))
                .unwrap_or(1);
            let sample_rate = item
                .virtual_state
                .as_ref()
                .map(|state| state.sample_rate)
                .or_else(|| item.meta.as_ref().map(|m| m.sample_rate))
                .unwrap_or(self.audio.shared.out_sample_rate.max(1));
            let bits_per_sample = item
                .virtual_state
                .as_ref()
                .map(|state| state.bits_per_sample)
                .or_else(|| item.meta.as_ref().map(|m| m.bits_per_sample))
                .unwrap_or(32);
            // The destination is filled in by the worker once it has hashed
            // the audio, and patched into both the asset and the virtual
            // item by their shared asset id.
            let asset_slot = SidecarSlot::ManagedAsset {
                asset_id: item.audio_asset.id.to_hex(),
                revision: item.audio_asset.revision.0.max(1),
            };
            let mut has_asset_audio = true;
            if let Some(audio) = current_audio.as_ref() {
                sidecar_jobs.push(SessionSidecarJob {
                    slot: asset_slot,
                    source: SessionSidecarSource::Buffer(audio.clone()),
                    sample_rate,
                    label: "managed virtual asset",
                });
            } else if let Some(source_path) = item.audio_asset.backing.file_path() {
                sidecar_jobs.push(SessionSidecarJob {
                    slot: asset_slot,
                    source: SessionSidecarSource::File(source_path.to_path_buf()),
                    sample_rate,
                    label: "managed virtual asset",
                });
            } else {
                has_asset_audio = false;
            }
            // No audio to write means no file to point at. Recording an
            // asset whose `location` the worker will never fill in would
            // write a manifest entry naming nothing.
            if has_asset_audio {
                assets.push(ProjectAsset {
                    id: item.audio_asset.id.to_hex(),
                    revision: item.audio_asset.revision.0.max(1),
                    item_path: if item.path.to_string_lossy().contains("://") {
                        item.path.to_string_lossy().to_string()
                    } else {
                        session_path(&item.path, base_dir, path_mode)
                    },
                    backing: "managed".to_string(),
                    // Filled in by the save worker, keyed by `id`.
                    location: String::new(),
                    sample_rate,
                    channels,
                    bits_per_sample,
                    frame_count: item.audio_asset.frame_count,
                });
            }
            virtual_items.push(ProjectVirtualItem {
                path: if item.path.to_string_lossy().contains("://") {
                    item.path.to_string_lossy().to_string()
                } else {
                    session_path(&item.path, base_dir, path_mode)
                },
                display_name: item.display_name.clone(),
                sample_rate,
                channels,
                bits_per_sample,
                source,
                op_chain,
                // Filled in by the save worker alongside the asset location.
                sidecar_audio: None,
                asset_id: Some(item.audio_asset.id.to_hex()),
                asset_revision: Some(item.audio_asset.revision.0.max(1)),
            });
        }
        for item in self
            .items
            .iter()
            .filter(|item| item.source == MediaSource::File)
        {
            let Some(location) = item.audio_asset.backing.file_path() else {
                continue;
            };
            assets.push(ProjectAsset {
                id: item.audio_asset.id.to_hex(),
                revision: item.audio_asset.revision.0.max(1),
                item_path: session_path(&item.path, base_dir, path_mode),
                backing: "external".to_string(),
                location: session_path(location, base_dir, path_mode),
                sample_rate: item.audio_asset.sample_rate,
                channels: item.audio_asset.channels,
                bits_per_sample: item.audio_asset.bits_per_sample,
                frame_count: item.audio_asset.frame_count,
            });
        }
        assets.sort_by(|left, right| left.item_path.cmp(&right.item_path));
        virtual_items.sort_by(|a, b| a.path.cmp(&b.path));
        let mut transcript_languages: Vec<ProjectTranscriptLanguage> = self
            .items
            .iter()
            .filter_map(|item| {
                item.transcript_language
                    .as_ref()
                    .map(|lang| (item.path.clone(), lang.trim().to_ascii_lowercase()))
            })
            .filter(|(_, lang)| !lang.is_empty())
            .map(|(path, language)| ProjectTranscriptLanguage {
                path: session_path(&path, base_dir, path_mode),
                language,
            })
            .collect();
        transcript_languages.sort_by(|a, b| a.path.cmp(&b.path));
        let transcripts: Vec<ProjectTranscriptDocument> = self
            .items
            .iter()
            .filter_map(|item| {
                item.transcript_document
                    .as_ref()
                    .map(|document| ProjectTranscriptDocument {
                        item_path: session_path(&item.path, base_dir, path_mode),
                        document: document.as_ref().clone(),
                    })
                    .or_else(|| {
                        item.transcript
                            .as_ref()
                            .map(|transcript| ProjectTranscriptDocument {
                                item_path: session_path(&item.path, base_dir, path_mode),
                                document: super::types::TranscriptDocument::from_transcript(
                                    transcript,
                                    item.transcript_language.clone(),
                                    item.audio_asset.id,
                                    item.audio_asset.revision,
                                ),
                            })
                    })
            })
            .collect();
        let list = ProjectList {
            root: self
                .root
                .as_ref()
                .map(|p| session_path(p, base_dir, path_mode)),
            files: list_files
                .iter()
                .map(|p| session_path(p, base_dir, path_mode))
                .collect(),
            items: list_items,
            sample_rate_overrides,
            bit_depth_overrides,
            format_overrides,
            virtual_items,
            transcript_languages,
            statuses: crate::app::status_tags::palette_to_project(&self.status_palette),
            tags: crate::app::status_tags::palette_to_project(&self.tag_palette),
            default_status: self.default_status.as_deref().map(str::to_string),
        };
        let key_column = self
            .external_key_index
            .and_then(|idx| self.external_headers.get(idx))
            .cloned();
        let external_state = ProjectExternalState {
            sources: self
                .external_sources
                .iter()
                .map(|src| ProjectExternalSource {
                    path: session_path(&src.path, base_dir, path_mode),
                    sheet_name: src.sheet_name.clone(),
                    has_header: src.has_header,
                    header_row: src.header_row,
                    data_row: src.data_row,
                })
                .collect(),
            active_source: self.external_active_source,
            key_rule: external_key_rule_to_project(self.external_key_rule).to_string(),
            match_input: external_match_input_to_project(self.external_match_input).to_string(),
            match_regex: self.external_match_regex.clone(),
            match_replace: self.external_match_replace.clone(),
            scope_regex: self.external_scope_regex.clone(),
            visible_columns: self.external_visible_columns.clone(),
            show_unmatched: self.external_show_unmatched,
            key_column,
        };
        let app = ProjectApp {
            theme: match self.theme_mode {
                super::types::ThemeMode::Light => "light".to_string(),
                _ => "dark".to_string(),
            },
            sort_key: match self.sort_key {
                super::types::SortKey::File => "File".to_string(),
                super::types::SortKey::Folder => "Folder".to_string(),
                super::types::SortKey::Transcript => "Transcript".to_string(),
                super::types::SortKey::Type => "Type".to_string(),
                super::types::SortKey::Length => "Length".to_string(),
                super::types::SortKey::Channels => "Channels".to_string(),
                super::types::SortKey::SampleRate => "SampleRate".to_string(),
                super::types::SortKey::Bits => "Bits".to_string(),
                super::types::SortKey::BitRate => "BitRate".to_string(),
                super::types::SortKey::Level => "Level".to_string(),
                super::types::SortKey::Lufs => "Lufs".to_string(),
                super::types::SortKey::TruePeak => "TruePeak".to_string(),
                super::types::SortKey::LufsShort => "LufsShort".to_string(),
                super::types::SortKey::LufsMomentary => "LufsMomentary".to_string(),
                super::types::SortKey::Bpm => "Bpm".to_string(),
                super::types::SortKey::SilenceLead => "SilenceLead".to_string(),
                super::types::SortKey::SilenceTail => "SilenceTail".to_string(),
                super::types::SortKey::EdgeZero => "EdgeZero".to_string(),
                super::types::SortKey::OverPeak => "OverPeak".to_string(),
                super::types::SortKey::BlankPad => "BlankPad".to_string(),
                super::types::SortKey::CreatedAt => "CreatedAt".to_string(),
                super::types::SortKey::ModifiedAt => "ModifiedAt".to_string(),
                super::types::SortKey::Comments => "Comments".to_string(),
                super::types::SortKey::External(_) => "External".to_string(),
                super::types::SortKey::Metadata(index) => self
                    .metadata_list_columns
                    .get(index)
                    .map(|column| column.key.serialized_name())
                    .unwrap_or_else(|| "File".to_string()),
            },
            sort_dir: match self.sort_dir {
                super::types::SortDir::Asc => "Asc",
                super::types::SortDir::Desc => "Desc",
                super::types::SortDir::None => "None",
            }
            .to_string(),
            search_query: self.search_query.clone(),
            search_regex: self.search_use_regex,
            selected_path: self
                .selected_path_buf()
                .as_ref()
                .map(|path| session_path(path, base_dir, path_mode)),
            list_columns: ProjectListColumns {
                edited: self.list_columns.edited,
                cover_art: self.list_columns.cover_art,
                type_badge: self.list_columns.type_badge,
                file: self.list_columns.file,
                folder: self.list_columns.folder,
                transcript: self.list_columns.transcript,
                transcript_language: self.list_columns.transcript_language,
                external: self.list_columns.external,
                status: self.list_columns.status,
                tags: self.list_columns.tags,
                length: self.list_columns.length,
                ch: self.list_columns.channels,
                sr: self.list_columns.sample_rate,
                bits: self.list_columns.bits,
                bit_rate: self.list_columns.bit_rate,
                peak: self.list_columns.peak,
                lufs: self.list_columns.lufs,
                dbtp: self.list_columns.dbtp,
                lufs_s: self.list_columns.lufs_s,
                lufs_m: self.list_columns.lufs_m,
                bpm: self.list_columns.bpm,
                created_at: self.list_columns.created_at,
                modified_at: self.list_columns.modified_at,
                gain: self.list_columns.gain,
                wave: self.list_columns.wave,
                comments: self.list_columns.comments,
                note: self.list_columns.note,
                silence_lead: self.list_columns.silence_lead,
                silence_tail: self.list_columns.silence_tail,
                edge_zero: self.list_columns.edge_zero,
                over_peak: self.list_columns.over_peak,
                blank_pad: self.list_columns.blank_pad,
                order: self
                    .list_column_layout
                    .iter()
                    .map(|key| key.serialized_name())
                    .collect(),
                widths: {
                    let mut widths: Vec<(String, f32)> = self
                        .list_col_widths
                        .iter()
                        .map(|(k, v)| (k.clone(), *v))
                        .collect();
                    widths.sort_by(|a, b| a.0.cmp(&b.0));
                    widths
                },
                metadata: self
                    .metadata_list_columns
                    .iter()
                    .map(|column| super::project::ProjectMetadataColumn {
                        key: column.key.serialized_name(),
                        label: column.label.clone(),
                        visible: column.visible,
                        width: column.width,
                    })
                    .collect(),
            },
            list_columns_window_pos: self
                .list_columns_window_pos
                .filter(|pos| pos.x.is_finite() && pos.y.is_finite())
                .map(|pos| [pos.x, pos.y]),
            auto_play_list_nav: self.auto_play_list_nav,
            export_policy: Some(ProjectExportPolicy {
                save_mode: match self.export_cfg.save_mode {
                    super::types::SaveMode::Overwrite => "overwrite".to_string(),
                    super::types::SaveMode::NewFile => "new_file".to_string(),
                },
                conflict: match self.export_cfg.conflict {
                    super::types::ConflictPolicy::Rename => "rename".to_string(),
                    super::types::ConflictPolicy::Overwrite => "overwrite".to_string(),
                    super::types::ConflictPolicy::Skip => "skip".to_string(),
                },
                backup_bak: self.export_cfg.backup_bak,
                export_srt: self.export_cfg.export_srt,
                name_template: self.export_cfg.name_template.clone(),
                dest_folder: self
                    .export_cfg
                    .dest_folder
                    .as_ref()
                    .map(|path| session_path(path, base_dir, path_mode)),
            }),
            external_state: Some(external_state),
            effect_graph_ui: Some(ProjectEffectGraphUi {
                tab_open: self.effect_graph.workspace_open,
                active_template_id: self.effect_graph.active_template_id.clone(),
            }),
            transcript_ai_config: Some(self.transcript_ai_cfg.clone()),
        };
        let spectrogram = project_spectrogram_from_cfg(&self.spectro_cfg);

        let mut tabs = Vec::new();
        for (idx, tab) in self.tabs.iter().enumerate() {
            let mut preview_tool = None;
            // Sidecar paths are left empty here and filled in by the save
            // worker: the filename is the audio's own hash, and hashing a
            // dirty tab's buffer on the UI thread would stall the frame.
            if tab.dirty && !tab.ch_samples.is_empty() {
                let sidecar_sr = tab.buffer_sample_rate.max(1);
                sidecar_jobs.push(SessionSidecarJob {
                    slot: SidecarSlot::TabEdited(idx),
                    source: SessionSidecarSource::Channels(tab.ch_samples_arc.clone()),
                    sample_rate: sidecar_sr,
                    label: "edited audio",
                });
            }
            if let Some(overlay) = tab.preview_overlay.as_ref() {
                if overlay.is_full_sample() {
                    sidecar_jobs.push(SessionSidecarJob {
                        slot: SidecarSlot::TabPreview(idx),
                        source: SessionSidecarSource::Channels(std::sync::Arc::new(
                            overlay.channels.clone(),
                        )),
                        sample_rate: self.audio.shared.out_sample_rate,
                        label: "preview audio",
                    });
                    preview_tool = Some(format!("{:?}", overlay.source_tool));
                }
            } else if let Some(tool) = tab.preview_audio_tool {
                preview_tool = Some(format!("{:?}", tool));
            }
            let entry = project_tab_from_tab(tab, base_dir, path_mode, None, None, preview_tool);
            tabs.push(entry);
        }

        let mut cached_edits = Vec::new();
        for (item_path, cached) in self.edited_cache.iter() {
            if cached.ch_samples.is_empty() {
                continue;
            }
            let sidecar_sr = cached.buffer_sample_rate.max(1);
            sidecar_jobs.push(crate::app::types::SessionSidecarJob {
                slot: SidecarSlot::CachedEdit(cached_edits.len()),
                source: crate::app::types::SessionSidecarSource::Channels(std::sync::Arc::new(
                    cached.ch_samples.clone(),
                )),
                sample_rate: sidecar_sr,
                label: "cached audio",
            });
            cached_edits.push(ProjectEdit {
                path: session_path(item_path, base_dir, path_mode),
                // Filled in by the worker, alongside the tab sidecars.
                edited_audio: String::new(),
                buffer_sample_rate: Some(cached.buffer_sample_rate.max(1)),
                dirty: cached.dirty,
                loop_region: cached.loop_region.map(|v| [v.0, v.1]),
                loop_markers_saved: cached.loop_markers_saved.map(|v| [v.0, v.1]),
                loop_markers_dirty: cached.loop_markers_dirty,
                markers: cached.markers.iter().map(marker_entry_to_project).collect(),
                regions: cached.regions.iter().map(region_entry_to_project).collect(),
                markers_saved: cached
                    .markers_saved
                    .iter()
                    .map(marker_entry_to_project)
                    .collect(),
                markers_dirty: cached.markers_dirty,
                trim_range: cached.trim_range.map(|v| [v.0, v.1]),
                loop_xfade_samples: cached.loop_xfade_samples,
                loop_xfade_shape: match cached.loop_xfade_shape {
                    LoopXfadeShape::Linear => "linear",
                    LoopXfadeShape::EqualPower => "equal",
                    LoopXfadeShape::LinearDip => "linear_dip",
                    LoopXfadeShape::EqualPowerDip => "equal_dip",
                }
                .to_string(),
                fade_in_range: cached.fade_in_range.map(|v| [v.0, v.1]),
                fade_out_range: cached.fade_out_range.map(|v| [v.0, v.1]),
                fade_in_shape: format!("{:?}", cached.fade_in_shape),
                fade_out_shape: format!("{:?}", cached.fade_out_shape),
                loop_mode: format!("{:?}", cached.loop_mode),
                tool_state: ProjectToolState {
                    fade_in_ms: cached.tool_state.fade_in_ms,
                    fade_out_ms: cached.tool_state.fade_out_ms,
                    gain_db: cached.tool_state.gain_db,
                    normalize_target_db: cached.tool_state.normalize_target_db,
                    loudness_target_lufs: cached.tool_state.loudness_target_lufs,
                    pitch_semitones: cached.tool_state.pitch_semitones,
                    stretch_rate: cached.tool_state.stretch_rate,
                    speed_rate: cached.tool_state.speed_rate,
                    warp_time_radius_ms: cached.tool_state.warp_time_radius_ms,
                    warp_freq_radius_hz: cached.tool_state.warp_freq_radius_hz,
                    loop_repeat: cached.tool_state.loop_repeat,
                    noise_gate_threshold_db: cached.tool_state.noise_gate_threshold_db,
                    noise_gate_attack_ms: cached.tool_state.noise_gate_attack_ms,
                    noise_gate_release_ms: cached.tool_state.noise_gate_release_ms,
                    eq_low_shelf_freq_hz: cached.tool_state.eq_low_shelf_freq_hz,
                    eq_low_shelf_gain_db: cached.tool_state.eq_low_shelf_gain_db,
                    eq_mid_freq_hz: cached.tool_state.eq_mid_freq_hz,
                    eq_mid_gain_db: cached.tool_state.eq_mid_gain_db,
                    eq_mid_q: cached.tool_state.eq_mid_q,
                    eq_high_shelf_freq_hz: cached.tool_state.eq_high_shelf_freq_hz,
                    eq_high_shelf_gain_db: cached.tool_state.eq_high_shelf_gain_db,
                    compressor_threshold_db: cached.tool_state.compressor_threshold_db,
                    compressor_ratio: cached.tool_state.compressor_ratio,
                    compressor_attack_ms: cached.tool_state.compressor_attack_ms,
                    compressor_release_ms: cached.tool_state.compressor_release_ms,
                    compressor_makeup_db: cached.tool_state.compressor_makeup_db,
                },
                active_tool: format!("{:?}", cached.active_tool),
                show_waveform_overlay: cached.show_waveform_overlay,
                bpm_enabled: cached.bpm_enabled,
                bpm_value: cached.bpm_value,
                bpm_user_set: cached.bpm_user_set,
                bpm_offset_sec: cached.bpm_offset_sec,
                time_sig_numerator: cached.time_sig_numerator,
                time_sig_denominator: cached.time_sig_denominator,
                plugin_fx_draft: project_plugin_fx_draft_from_draft(&cached.plugin_fx_draft),
                plugin_fx_chain: project_plugin_fx_chain_from_draft(&cached.plugin_fx_chain),
                applied_effect_graph: cached.applied_effect_graph.as_ref().map(|stamp| {
                    ProjectAppliedEffectGraph {
                        template_id: stamp.template_id.clone(),
                        template_name: stamp.template_name.clone(),
                        template_updated_at_unix_ms: stamp.template_updated_at_unix_ms,
                    }
                }),
                music_analysis: None,
            });
        }

        let project = ProjectFile {
            version: 2,
            // Carried across saves of the same document; `save_project_as`
            // to a different path forks it before the plan is built.
            session_id: Some(
                self.session_id
                    .get_or_insert_with(super::session_sync::new_session_id)
                    .clone(),
            ),
            // `revision` and `saved_at` are stamped on the worker, once it
            // has read what is actually on disk -- the new revision has to
            // follow the document it is replacing, not the one we loaded.
            revision: None,
            saved_at: None,
            saved_by: Some(self.session_saved_by()),
            assets,
            transcripts,
            name: path
                .file_stem()
                .and_then(|s| s.to_str())
                .map(|s| s.to_string()),
            path_mode: Some(path_mode.as_str().to_string()),
            base_dir: Some(base_dir.to_string_lossy().to_string()),
            list,
            app,
            spectrogram,
            tabs,
            active_tab: self.active_tab,
            cached_edits,
            // Whatever this process knows about. The worker unions in what is
            // on disk before it writes, so a colleague's comment posted since
            // we loaded is not dropped by our save.
            comments: self.comments.clone(),
        };
        Ok((path, project, sidecar_jobs))
    }

    /// The disk-touching half of a session save: sidecar WAV encodes, TOML
    /// serialization, and the session file write. Runs on a worker thread
    /// for interactive saves and inline for the blocking variant.
    ///
    /// `expected` is the fingerprint of the document this save is based on.
    /// When it is set and the file on disk no longer matches, **nothing is
    /// committed** and the caller gets a [`SessionSaveOutcome::Conflict`] --
    /// on a shared file server the mismatch is a colleague's save, and
    /// replacing it is silent data loss. `None` means the caller has no
    /// expectation: a Save As to a fresh path, or an overwrite the user
    /// asked for after seeing the conflict.
    ///
    /// The check runs twice. Once before the sidecar encodes, which can take
    /// seconds, so an already-doomed save fails fast; and once immediately
    /// before the document commit, which is the one that decides. A few
    /// milliseconds still separate that read from the rename -- closing that
    /// window needs a lock, and this design deliberately has none.
    fn run_session_save_jobs(
        path: &Path,
        project: &mut ProjectFile,
        jobs: &[crate::app::types::SessionSidecarJob],
        expected: Option<session_sync::SessionFingerprint>,
        expected_comment_free: Option<session_sync::SessionFingerprint>,
    ) -> Result<crate::app::types::SessionSaveOutcome, String> {
        use crate::app::types::{SessionSaveOutcome, SidecarSlot};

        let base_dir = path.parent().unwrap_or_else(|| Path::new("."));

        // Fail fast: do not spend seconds encoding WAVs for a document that
        // is already going to be refused.
        let disk = session_sync::read_session_state(path)
            .map_err(|err| format!("Failed to read {}: {err}", path.display()))?;
        if let Some(conflict) =
            Self::session_conflict_from(path, &disk, expected, expected_comment_free)
        {
            return Ok(SessionSaveOutcome::Conflict(conflict));
        }

        let nonce = Self::save_nonce();
        let mut staged = Vec::<(PathBuf, PathBuf)>::new();
        // Drop the staged sidecars if the save aborts. A leaked stage file is
        // visible junk in a folder the whole team looks at.
        let cleanup = |staged: &[(PathBuf, PathBuf)]| {
            for (pending, _) in staged {
                let _ = std::fs::remove_file(pending);
            }
        };

        let mut committed_assets = Vec::new();
        for job in jobs {
            // The sidecar is named after its own contents, so two people
            // saving the same shared session write to different files rather
            // than over each other's audio.
            let content_hash = match &job.source {
                crate::app::types::SessionSidecarSource::File(source) => {
                    match session_sync::hash_file_content(source) {
                        Ok(hash) => hash,
                        Err(error) => {
                            cleanup(&staged);
                            return Err(format!("Failed to read {}: {error}", job.label));
                        }
                    }
                }
                crate::app::types::SessionSidecarSource::Channels(_)
                | crate::app::types::SessionSidecarSource::Buffer(_) => {
                    session_sync::hash_audio_content(job.source.channels(), job.sample_rate)
                }
            };
            let dst = match &job.slot {
                SidecarSlot::ManagedAsset { asset_id, revision } => {
                    let Some(id) = crate::audio_asset::AudioAssetId::from_hex(asset_id) else {
                        // We wrote this id ourselves, so it round-trips.
                        // Skipping the job would leave the manifest entry
                        // pointing at nothing, so fail the save instead.
                        cleanup(&staged);
                        return Err(format!("Malformed audio asset id: {asset_id}"));
                    };
                    super::project::asset_audio_dst(
                        path,
                        id,
                        crate::audio_asset::AssetRevision(*revision),
                        &content_hash,
                    )
                }
                _ => super::project::sidecar_audio_dst(path, &content_hash),
            };
            let reference = rel_path(&dst, base_dir);
            match &job.slot {
                SidecarSlot::ManagedAsset { asset_id, .. } => {
                    // Both records name the same file; they are keyed rather
                    // than indexed because the plan sorts them afterwards.
                    for asset in project
                        .assets
                        .iter_mut()
                        .filter(|asset| &asset.id == asset_id)
                    {
                        asset.location = reference.clone();
                    }
                    for item in project
                        .list
                        .virtual_items
                        .iter_mut()
                        .filter(|item| item.asset_id.as_deref() == Some(asset_id.as_str()))
                    {
                        item.sidecar_audio = Some(reference.clone());
                    }
                    if let Some(id) = crate::audio_asset::AudioAssetId::from_hex(asset_id) {
                        committed_assets.push((id, dst.clone()));
                    }

                }
                SidecarSlot::TabEdited(idx) => {
                    if let Some(tab) = project.tabs.get_mut(*idx) {
                        tab.edited_audio = Some(reference);
                    }
                }
                SidecarSlot::TabPreview(idx) => {
                    if let Some(tab) = project.tabs.get_mut(*idx) {
                        tab.preview_audio = Some(reference);
                    }
                }
                SidecarSlot::CachedEdit(idx) => {
                    if let Some(edit) = project.cached_edits.get_mut(*idx) {
                        edit.edited_audio = reference;
                    }
                }
            }
            if let Some(parent) = dst.parent() {
                if let Err(error) = std::fs::create_dir_all(parent) {
                    cleanup(&staged);
                    return Err(format!("Failed to save {}: {error}", job.label));
                }
            }
            // Content addressing makes a re-save of unchanged audio a no-op:
            // the bytes already on disk are the bytes we would write.
            if dst.is_file() {
                continue;
            }
            let stage = dst.with_extension(format!("wav.{nonce}.stage"));
            let result = match &job.source {
                crate::app::types::SessionSidecarSource::File(source) => {
                    if std::fs::hard_link(source, &stage).is_ok() {
                        Ok(())
                    } else {
                        std::fs::copy(source, &stage)
                            .map(|_| ())
                            .map_err(anyhow::Error::from)
                    }
                }
                crate::app::types::SessionSidecarSource::Channels(_)
                | crate::app::types::SessionSidecarSource::Buffer(_) => {
                    let channels = job.source.channels();
                    let len = channels.first().map(Vec::len).unwrap_or(0);
                    crate::wave::export_selection_wav(channels, job.sample_rate, (0, len), &stage)
                }
            };
            if let Err(error) = result {
                cleanup(&staged);
                let _ = std::fs::remove_file(&stage);
                return Err(format!("Failed to stage {}: {error}", job.label));
            }
            staged.push((stage, dst));
        }

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }

        // The check that decides. Everything above is reversible; from here
        // on the document is replaced.
        let disk = session_sync::read_session_state(path)
            .map_err(|err| format!("Failed to read {}: {err}", path.display()))?;
        if let Some(conflict) =
            Self::session_conflict_from(path, &disk, expected, expected_comment_free)
        {
            cleanup(&staged);
            return Ok(SessionSaveOutcome::Conflict(conflict));
        }

        // Take the conversation on disk with us. The bytes are already in
        // hand for the check above, and comments merge as a set union, so a
        // colleague's post since we loaded costs nothing to carry -- while
        // dropping it would be silent data loss on exactly the path a user
        // reaches by choosing Overwrite. A document we cannot parse (only
        // possible on a deliberate overwrite, since the check above refuses
        // everything else) has nothing to contribute, and is not a reason to
        // fail a save.
        if let Some(bytes) = disk.bytes() {
            if let Ok(theirs) = std::str::from_utf8(bytes).map_err(|_| ()).and_then(|text| {
                super::project::deserialize_project(text).map_err(|_| ())
            }) {
                super::comments::merge_into(&mut project.comments, theirs.comments);
            }
        }

        // Stamp the document against what is actually on disk, so the
        // revision keeps climbing even across a forced overwrite.
        let stamp = disk.stamp();
        let revision = stamp.revision.unwrap_or(0).saturating_add(1);
        let session_id = project
            .session_id
            .clone()
            .unwrap_or_else(session_sync::new_session_id);
        project.session_id = Some(session_id.clone());
        project.revision = Some(revision);
        project.saved_at = Some(session_sync::now_rfc3339());

        let text = serialize_project(project).map_err(|e| e.to_string())?;

        // Replacing a document we had no expectation for -- a deliberate
        // overwrite, or a Save As onto an existing file -- keeps one copy of
        // what was there. It is the only way back if the overwrite was a
        // mistake, and someone else's day of work is on the other side of it.
        if expected.is_none() {
            if let Some(previous) = disk.bytes() {
                let backup = Self::session_backup_path(path);
                let backup_temp = backup.with_extension(format!("bak.{nonce}.tmp"));
                let backed_up = std::fs::write(&backup_temp, previous)
                    .and_then(|()| session_sync::atomic_replace_file(&backup_temp, &backup));
                if backed_up.is_err() {
                    let _ = std::fs::remove_file(&backup_temp);
                }
            }
        }

        for (index, (stage, destination)) in staged.iter().enumerate() {
            if let Err(error) = session_sync::atomic_replace_file(stage, destination) {
                // Whatever committed before this point is additive -- the
                // names are content hashes, so nothing was replaced -- and
                // the document is not written, so nothing references them.
                // Drop the stages that never made it.
                cleanup(&staged[index..]);
                return Err(format!(
                    "Failed to commit session asset {}: {error}",
                    destination.display()
                ));
            }
        }

        let session_temp = path.with_extension(format!("nwsess.{nonce}.tmp"));
        session_sync::retry_shared_io(|| std::fs::write(&session_temp, &text))
            .map_err(|e| e.to_string())?;
        if let Err(error) = session_sync::atomic_replace_file(&session_temp, path) {
            let _ = std::fs::remove_file(&session_temp);
            return Err(error.to_string());
        }

        Self::sweep_stale_session_temps(path);

        let comment_free_fingerprint =
            super::project::comment_free_fingerprint(project).map_err(|e| e.to_string())?;
        Ok(SessionSaveOutcome::Saved {
            path: path.to_path_buf(),
            fingerprint: session_sync::SessionFingerprint::of_bytes(text.as_bytes()),
            session_id,
            revision,
            previous: disk.bytes().map(|bytes| bytes.to_vec()),
            previous_revision: stamp.revision,
            previous_saved_by: stamp.saved_by.clone(),
            previous_saved_at: stamp.saved_at.clone(),
            committed_assets,
            comments: std::mem::take(&mut project.comments),
            comment_free_fingerprint,
        })
    }

    /// Marks a name as one of ours, so the stale-temp sweep can never
    /// mistake somebody's unrelated `.tmp` for garbage of its own making.
    pub(super) const SAVE_TEMP_MARKER: &'static str = "nwtmp";

    /// A temp-name suffix no other writer can produce. The pid alone is not
    /// enough on a share: two machines hand out the same pids.
    pub(super) fn save_nonce() -> String {
        format!(
            "{}{}.{}.{}",
            Self::SAVE_TEMP_MARKER,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
            &session_sync::new_session_id()[..8]
        )
    }

    pub(super) fn session_backup_path(path: &Path) -> PathBuf {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "session.nwsess".to_string());
        path.with_file_name(format!("{name}.bak"))
    }

    /// Decide whether the document on disk is still the one this save is
    /// based on. `None` means "go ahead".
    ///
    /// `expected_comment_free` is the same document hashed without its
    /// conversation, and it is what keeps a colleague's comment from turning
    /// the author's next Ctrl+S into a conflict prompt. Posting rewrites the
    /// shared file, so without this every comment would leave everyone else's
    /// save refused until they reloaded -- and reloading discards unsaved
    /// edits. A document that differs from ours *only* in what people said is
    /// not a document to be afraid of overwriting: the save merges the
    /// conversation in rather than replacing it.
    fn session_conflict_from(
        path: &Path,
        disk: &session_sync::SessionDiskState,
        expected: Option<session_sync::SessionFingerprint>,
        expected_comment_free: Option<session_sync::SessionFingerprint>,
    ) -> Option<crate::app::types::SessionConflict> {
        let expected = expected?;
        match disk.fingerprint() {
            // Gone. Recreating it is what the user asked for, and refusing
            // would strand their edits with nowhere to go.
            None => None,
            Some(actual) if actual == expected => None,
            Some(_) => {
                if Self::disk_differs_only_by_comments(disk, expected_comment_free) {
                    return None;
                }
                let stamp = disk.stamp();
                Some(crate::app::types::SessionConflict {
                    path: path.to_path_buf(),
                    on_disk: stamp.describe(),
                    based_on_revision: None,
                    close_when_resolved: false,
                })
            }
        }
    }

    /// Whether the only thing that moved on disk is the conversation.
    ///
    /// Deliberately conservative: a document we cannot read, or a caller with
    /// no comment-free hash to compare against, answers "no" and the save is
    /// refused as before. Being wrong in that direction costs a prompt; being
    /// wrong the other way costs somebody's work.
    fn disk_differs_only_by_comments(
        disk: &session_sync::SessionDiskState,
        expected_comment_free: Option<session_sync::SessionFingerprint>,
    ) -> bool {
        let Some(expected_comment_free) = expected_comment_free else {
            return false;
        };
        let Some(bytes) = disk.bytes() else {
            return false;
        };
        let Ok(text) = std::str::from_utf8(bytes) else {
            return false;
        };
        let Ok(mut theirs) = deserialize_project(text) else {
            return false;
        };
        super::project::comment_free_fingerprint(&mut theirs)
            .is_ok_and(|actual| actual == expected_comment_free)
    }

    /// Remove staging leftovers from a save that died before it could clean
    /// up after itself. Only unambiguous garbage: our own temp suffixes, and
    /// only once they are old enough that no save in flight -- ours or
    /// another machine's -- could still be using them.
    ///
    /// Committed sidecars are never touched. On a shared session another
    /// person's current document may reference audio this one does not, and
    /// nothing here can tell the difference between "orphaned" and "theirs".
    fn sweep_stale_session_temps(path: &Path) {
        const STALE_AFTER: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);
        // The session usually sits in the folder it lists, which in this app
        // can hold a hundred thousand files. Walking all of it on every save
        // would cost more than the save. `read_dir` is lazy, so stopping
        // early actually stops the work.
        const MAX_ENTRIES_SCANNED: usize = 4096;
        let mut dirs = Vec::new();
        if let Some(parent) = path.parent() {
            dirs.push(parent.to_path_buf());
        }
        dirs.push(super::project::project_data_dir(path));
        let now = std::time::SystemTime::now();
        for dir in dirs {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.take(MAX_ENTRIES_SCANNED).flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                // The session's folder is a folder the user owns. Only names
                // carrying our own marker are ours to delete -- somebody
                // else's stale `.tmp` in there is none of our business.
                if !name.contains(Self::SAVE_TEMP_MARKER) {
                    continue;
                }
                if !name.ends_with(".stage") && !name.ends_with(".tmp") {
                    continue;
                }
                let old = entry
                    .metadata()
                    .and_then(|meta| meta.modified())
                    .ok()
                    .and_then(|modified| now.duration_since(modified).ok())
                    .map(|age| age > STALE_AFTER)
                    .unwrap_or(false);
                if old {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
    }

    /// Re-point virtual items at the managed audio the save just committed.
    ///
    /// The worker reports where each asset actually landed rather than the
    /// UI re-deriving the name and stat-ing it: the name now carries a
    /// content hash the UI thread has no cheap way to know, and statting a
    /// path on a share from the UI thread is exactly what `AGENTS.md`
    /// forbids.
    fn finish_session_save(
        &mut self,
        path: PathBuf,
        committed_assets: &[(crate::audio_asset::AudioAssetId, PathBuf)],
    ) {
        for item in self
            .items
            .iter_mut()
            .filter(|item| item.source == MediaSource::Virtual)
        {
            if let Some((_, managed)) = committed_assets
                .iter()
                .find(|(id, _)| *id == item.audio_asset.id)
            {
                item.audio_asset.backing =
                    crate::audio_asset::AudioBacking::ManagedFile(managed.clone());
            }
        }
        self.project_path = Some(path.clone());
        self.add_recent_session_path(&path);
        // Recorded virtual items have just been persisted as sidecar audio;
        // their backing temp WAVs in %TEMP% are no longer needed.
        self.clear_recording_temp_files();
    }

    /// Interactive save: snapshots everything cheaply, then encodes and
    /// writes on a worker while the busy overlay keeps the UI responsive.
    pub(super) fn save_project_as(&mut self, path: PathBuf) -> Result<(), String> {
        self.save_project_as_forced(path, false)
    }

    /// `force` skips the compare-and-swap. Only the conflict prompt sets it,
    /// after the user has been shown whose save they are replacing.
    pub(super) fn save_project_as_forced(
        &mut self,
        path: PathBuf,
        force: bool,
    ) -> Result<(), String> {
        if self.session_save_state.is_some() {
            return Err("session save already in progress".to_string());
        }
        // Saving a partly-restored document would write the half of it that
        // has been applied over the complete file on disk.
        if self.session_open_in_progress() {
            return Err("session is still opening".to_string());
        }
        let (path, mut project, jobs) = self.build_session_save_plan(path)?;
        let job_count = jobs.len();
        let expected = self.expected_fingerprint_for(&path, force);
        // Only meaningful alongside an expectation: a deliberate overwrite
        // has nothing to compare against and wants none.
        let expected_comment_free = expected.and(self.session_comment_free_fingerprint);
        let (tx, rx) = std::sync::mpsc::channel();
        let worker_path = path.clone();
        std::thread::spawn(move || {
            crate::app::threading::lower_current_thread_priority();
            let result = Self::run_session_save_jobs(
                &worker_path,
                &mut project,
                &jobs,
                expected,
                expected_comment_free,
            );
            let _ = tx.send(result);
            // The frame loop sleeps when idle; without this the save result
            // sits in the channel until the user moves the mouse.
            crate::ui_wake::wake_ui();
        });
        self.session_save_state = Some(crate::app::types::SessionSaveState {
            msg: if job_count == 0 {
                "Saving session...".to_string()
            } else {
                format!("Saving session... ({job_count} audio sidecars)")
            },
            rx,
            started_at: Instant::now(),
        });
        Ok(())
    }

    /// Synchronous save for flows that must observe completion (CLI, tests,
    /// close-with-autosave).
    ///
    /// A conflict is reported as an error here rather than a prompt: the
    /// callers are headless. It still never overwrites.
    pub(super) fn save_project_as_blocking(&mut self, path: PathBuf) -> Result<(), String> {
        self.save_project_as_blocking_forced(path, false)
    }

    pub(super) fn save_project_as_blocking_forced(
        &mut self,
        path: PathBuf,
        force: bool,
    ) -> Result<(), String> {
        let (path, mut project, jobs) = self.build_session_save_plan(path)?;
        let expected = self.expected_fingerprint_for(&path, force);
        // Only meaningful alongside an expectation: a deliberate overwrite
        // has nothing to compare against and wants none.
        let expected_comment_free = expected.and(self.session_comment_free_fingerprint);
        match Self::run_session_save_jobs(&path, &mut project, &jobs, expected, expected_comment_free)? {
            crate::app::types::SessionSaveOutcome::Saved {
                path,
                fingerprint,
                session_id,
                revision,
                previous,
                previous_revision,
                previous_saved_by,
                previous_saved_at,
                committed_assets,
                comments,
                comment_free_fingerprint,
            } => {
                let replaced = path.clone();
                self.adopt_saved_session(
                    path,
                    fingerprint,
                    session_id,
                    revision,
                    &committed_assets,
                    comments,
                    comment_free_fingerprint,
                );
                // After adopting, not before: a Save As clears `session_id`
                // while it builds the plan, so capturing first would file the
                // document under a key the history window never asks for --
                // and "undo the Save As I just did" is exactly what somebody
                // opens that window for.
                self.capture_replaced_session_version(
                    &replaced,
                    previous,
                    previous_revision,
                    previous_saved_by,
                    previous_saved_at,
                );
                Ok(())
            }
            crate::app::types::SessionSaveOutcome::Conflict(conflict) => Err(format!(
                "Session changed on disk since it was opened ({}) — nothing was written",
                conflict.on_disk
            )),
        }
    }

    /// What this save is allowed to assume is on disk.
    ///
    /// `None` -- no expectation, commit unconditionally -- for a Save As to
    /// a different path (nothing of ours is there to protect) and for an
    /// overwrite the user chose after seeing the conflict.
    fn expected_fingerprint_for(
        &mut self,
        path: &Path,
        force: bool,
    ) -> Option<session_sync::SessionFingerprint> {
        if force {
            return None;
        }
        if self.project_path.as_deref() != Some(path) {
            return None;
        }
        if self.session_disk_fingerprint.is_none() {
            // Saving over the session we have open without knowing what we
            // read from it. Every path that sets `project_path` also records
            // the fingerprint, so this should be unreachable -- but if it
            // ever is reached the save proceeds unchecked, which is exactly
            // the behavior this feature exists to remove. Make it visible.
            self.debug_log(
                "session save has no fingerprint for the open document; the overwrite check is being skipped"
                    .to_string(),
            );
        }
        self.session_disk_fingerprint
    }

    /// Record what we just wrote, so the watch does not report our own save
    /// as somebody else's and the next save has something to compare with.
    #[allow(clippy::too_many_arguments)]
    fn adopt_saved_session(
        &mut self,
        path: PathBuf,
        fingerprint: session_sync::SessionFingerprint,
        session_id: String,
        revision: u64,
        committed_assets: &[(crate::audio_asset::AudioAssetId, PathBuf)],
        comments: Vec<super::project::ProjectComment>,
        comment_free_fingerprint: session_sync::SessionFingerprint,
    ) {
        self.session_disk_fingerprint = Some(fingerprint);
        self.session_comment_free_fingerprint = Some(comment_free_fingerprint);
        // What was committed, not what we set out to commit: the worker
        // unioned in whatever the document had gained since we loaded it.
        self.comments = comments;
        self.mark_comment_index_dirty();
        self.session_id = Some(session_id);
        self.session_revision = Some(revision);
        self.session_paths_repaired = false;
        self.session_changed_on_disk = None;
        self.session_changed_pending = None;
        self.session_conflict = None;
        // Everything queued went out inside this document: the save plan
        // copies `self.comments`, which the outbox has already been folded
        // into. Leaving them queued would post each of them a second time.
        self.comment_outbox.clear();
        self.finish_session_save(path, committed_assets);
        // Re-arm the watch against what we just wrote, so our own save is
        // never reported back as somebody else's.
        self.restart_session_watch();
    }

    pub(super) fn drain_session_save(&mut self, ctx: &egui::Context) {
        use crate::app::loading_ops::{poll_job, JobPoll};
        let result = match &self.session_save_state {
            Some(state) => match poll_job(&state.rx) {
                JobPoll::Ready(result) => Some(result),
                JobPoll::Waiting => None,
                JobPoll::Gone => {
                    // The worst of the modal states to lose: it blocks input,
                    // so the quit prompt behind it cannot be answered, and the
                    // save it is waiting for has already stopped happening.
                    // Nothing was written -- the document on disk is whatever
                    // it was -- so the only wrong move is to keep pretending.
                    self.session_save_state = None;
                    self.close_after_session_save = false;
                    self.push_toast(
                        crate::app::types::ToastSeverity::Error,
                        "Save did not finish — the session file was not written. \
                         Try saving again."
                            .to_string(),
                    );
                    self.debug_log("session save worker stopped without a result".to_string());
                    ctx.request_repaint();
                    return;
                }
            },
            None => None,
        };
        let Some(result) = result else {
            return;
        };
        self.session_save_state = None;
        let close_when_done = std::mem::take(&mut self.close_after_session_save);
        match result {
            Ok(crate::app::types::SessionSaveOutcome::Saved {
                path,
                fingerprint,
                session_id,
                revision,
                previous,
                previous_revision,
                previous_saved_by,
                previous_saved_at,
                committed_assets,
                comments,
                comment_free_fingerprint,
            }) => {
                self.debug_log(format!(
                    "session saved: {} (revision {revision}, {})",
                    path.display(),
                    fingerprint.short_hex()
                ));
                let replaced = path.clone();
                self.adopt_saved_session(
                    path,
                    fingerprint,
                    session_id,
                    revision,
                    &committed_assets,
                    comments,
                    comment_free_fingerprint,
                );
                // See the blocking path: the key has to be the session that
                // now lives at this path, which a Save As only settles here.
                self.capture_replaced_session_version(
                    &replaced,
                    previous,
                    previous_revision,
                    previous_saved_by,
                    previous_saved_at,
                );
                if close_when_done {
                    self.close_project();
                }
            }
            Ok(crate::app::types::SessionSaveOutcome::Conflict(mut conflict)) => {
                self.debug_log(format!(
                    "session save refused, document changed on disk: {}",
                    conflict.on_disk
                ));
                conflict.based_on_revision = self.session_revision;
                // A close that cannot write must not tear the session down:
                // the edits it failed to persist are still only in memory.
                conflict.close_when_resolved = close_when_done;
                self.session_conflict = Some(conflict);
            }
            Err(err) => {
                self.debug_log(format!("session save error: {err}"));
                self.push_toast(
                    super::types::ToastSeverity::Error,
                    if close_when_done {
                        format!("Session close autosave failed: {err}")
                    } else {
                        format!("Session save failed: {err}")
                    },
                );
            }
        }
        ctx.request_repaint();
    }

    /// Read and normalize a session document. Touches only the filesystem
    /// and the parsed document, never `self`, so the GUI runs it on a
    /// worker thread (see `tick_project_open`): the path repair alone
    /// stats every file the session references, which on a large session
    /// or a network share is seconds of blocking I/O.
    pub(super) fn parse_session_document(path: PathBuf) -> Result<ParsedSession, String> {
        let path = if path.is_absolute() {
            path
        } else {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(path)
        };
        let bytes = session_sync::read_session_bytes(&path)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("Session file not found: {}", path.display()))?;
        // A zero-length document is what an interrupted non-atomic write
        // leaves behind -- older builds and outside tools still produce
        // those. Say so, and point at the backup if one is there, rather
        // than reporting an unhelpful TOML parse error.
        if bytes.is_empty() {
            let backup = Self::session_backup_path(&path);
            return Err(if backup.is_file() {
                format!(
                    "Session file is empty — a previous save may have been interrupted. A backup is at {}",
                    backup.display()
                )
            } else {
                "Session file is empty — a previous save may have been interrupted".to_string()
            });
        }
        let fingerprint = session_sync::SessionFingerprint::of_bytes(&bytes);
        let text = String::from_utf8(bytes).map_err(|_| {
            format!("Session file is not valid UTF-8: {}", path.display())
        })?;
        let mut project = deserialize_project(&text).map_err(|e| e.to_string())?;
        if project.version != 1 && project.version != 2 {
            return Err(format!("Unsupported session version: {}", project.version));
        }
        let session_path_mode = SessionPathMode::from_project(&project);
        let path_repair = repair_project_source_paths(&mut project, &path);
        // Relative entries always follow the current `.nwsess` location.
        // Absolute entries have already been checked/repaired above.
        let base_dir = path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        let file_exists = project
            .list
            .files
            .iter()
            .map(|raw| resolve_path(raw, &base_dir).is_file())
            .collect();
        let other_exists = Self::probe_session_paths(&project, &base_dir);
        let comment_free_fingerprint = super::project::comment_free_fingerprint(&mut project)
            .map_err(|e| e.to_string())?;
        Ok(ParsedSession {
            path,
            project: Box::new(project),
            fingerprint,
            comment_free_fingerprint,
            path_repair,
            session_path_mode,
            base_dir,
            file_exists,
            other_exists,
        })
    }

    /// Stat every path the apply stage would otherwise have to check, while
    /// still on the parse worker. Each of these was a blocking syscall on
    /// the UI thread — one per tab, per virtual item, per external source —
    /// and on a share any one of them can stall for the SMB timeout.
    fn probe_session_paths(
        project: &ProjectFile,
        base_dir: &Path,
    ) -> rustc_hash::FxHashMap<PathBuf, bool> {
        let mut out: rustc_hash::FxHashMap<PathBuf, bool> = Default::default();
        let probe = |path: PathBuf, out: &mut rustc_hash::FxHashMap<PathBuf, bool>| {
            if let std::collections::hash_map::Entry::Vacant(slot) = out.entry(path.clone()) {
                slot.insert(path.is_file());
            }
        };
        for tab in &project.tabs {
            probe(resolve_path(&tab.path, base_dir), &mut out);
        }
        for entry in &project.list.virtual_items {
            if entry.source.kind.eq_ignore_ascii_case("file") {
                if let Some(raw) = entry.source.path.as_deref() {
                    probe(resolve_path(raw, base_dir), &mut out);
                }
            }
        }
        for manifest in &project.assets {
            probe(resolve_path(&manifest.location, base_dir), &mut out);
        }
        // External sources are checked with `exists()` rather than
        // `is_file()`, but a directory there is already unusable, so one
        // probe covers both.
        if let Some(external) = project.app.external_state.as_ref() {
            for source in &external.sources {
                probe(resolve_path(&source.path, base_dir), &mut out);
            }
        }
        out
    }

    /// Everything the restore will need to decode, in document order.
    /// Collected from the parsed document so the decodes can be spread over
    /// workers before any of them blocks a frame.
    fn collect_prefetch_requests(&self, parsed: &ParsedSession) -> Vec<PrefetchRequest> {
        let project = &parsed.project;
        let out_sr = self.audio.shared.out_sample_rate.max(1);
        let quality = Self::to_wave_resample_quality(self.src_quality);
        let mut requests: Vec<PrefetchRequest> = Vec::new();
        let mut seen_sidecars: std::collections::HashSet<String> = Default::default();
        let mut seen_files: std::collections::HashSet<PathBuf> = Default::default();
        let mut push_sidecar =
            |raw: &str, prep: Option<SidecarPrep>, requests: &mut Vec<PrefetchRequest>| {
                if raw.trim().is_empty() {
                    return;
                }
                if seen_sidecars.insert(raw.to_string()) {
                    requests.push(PrefetchRequest::Sidecar {
                        raw: raw.to_string(),
                        prep,
                    });
                }
            };
        // Virtual sidecars are consumed raw (the restore resamples them to
        // the item's own rate, not the output rate), so they get no prep.
        for entry in &project.list.virtual_items {
            if let Some(raw) = entry.sidecar_audio.as_deref() {
                push_sidecar(raw, None, &mut requests);
            }
            // Sidecar-kind sources keep their tag in source.path.
            if entry.source.kind.eq_ignore_ascii_case("sidecar") {
                if let Some(raw) = entry.source.path.as_deref() {
                    push_sidecar(raw, None, &mut requests);
                }
            }
        }
        // Cached edits and tab edits become editor buffers: prepare them.
        for edit in &project.cached_edits {
            let prep = SidecarPrep {
                stored_buffer_sr: edit.buffer_sample_rate,
                out_sr,
                quality,
            };
            push_sidecar(&edit.edited_audio, Some(prep), &mut requests);
        }
        for tab in &project.tabs {
            if let Some(raw) = tab.edited_audio.as_deref() {
                let prep = SidecarPrep {
                    stored_buffer_sr: tab.buffer_sample_rate,
                    out_sr,
                    quality,
                };
                push_sidecar(raw, Some(prep), &mut requests);
            }
            // The preview overlay is resampled to the output rate but has no
            // waveform cache of its own, so it stays a raw fetch.
            if let Some(raw) = tab.preview_audio.as_deref() {
                push_sidecar(raw, None, &mut requests);
            }
        }
        // Virtual items rebuilt from a raw file source, and managed assets
        // small enough to stay resident, are decoded from real paths.
        for entry in &project.list.virtual_items {
            if entry.sidecar_audio.is_some() {
                continue;
            }
            if entry.source.kind.eq_ignore_ascii_case("file") {
                if let Some(raw) = entry.source.path.as_deref() {
                    let path = resolve_path(raw, &parsed.base_dir);
                    if seen_files.insert(path.clone()) {
                        requests.push(PrefetchRequest::File { path });
                    }
                }
            }
        }
        requests
    }

    /// Decode `requests` across `concurrency` low-priority workers.
    ///
    /// Failures are simply absent from the result: every call site already
    /// had an error path for a decode that did not work (a missing sidecar,
    /// an unreadable source), and reproducing the message here would report
    /// it twice.
    fn run_audio_prefetch(
        project_path: &Path,
        requests: Vec<PrefetchRequest>,
        concurrency: usize,
        cancel: &std::sync::Arc<std::sync::atomic::AtomicBool>,
        progress: Option<std::sync::mpsc::Sender<PrefetchProgress>>,
    ) -> SessionAudioPrefetch {
        use std::sync::atomic::Ordering;

        let mut prefetch = SessionAudioPrefetch::default();
        if requests.is_empty() {
            return prefetch;
        }
        let queue = std::sync::Arc::new(std::sync::Mutex::new(
            requests
                .into_iter()
                .collect::<std::collections::VecDeque<_>>(),
        ));
        let (tx, rx) = std::sync::mpsc::channel::<PrefetchResult>();
        let workers = concurrency.max(1);
        let mut handles = Vec::with_capacity(workers);
        for _ in 0..workers {
            let queue = std::sync::Arc::clone(&queue);
            let tx = tx.clone();
            let cancel = std::sync::Arc::clone(cancel);
            let project_path = project_path.to_path_buf();
            let progress = progress.clone();
            let handle = std::thread::Builder::new()
                .name("neowaves-session-decode".to_string())
                .spawn(move || {
                    crate::app::threading::lower_current_thread_priority();
                    loop {
                        if cancel.load(Ordering::Relaxed) {
                            break;
                        }
                        let Some(request) =
                            queue.lock().unwrap_or_else(|e| e.into_inner()).pop_front()
                        else {
                            break;
                        };
                        // Name the item before touching it: the read below
                        // is where a load off a share appears to hang, and
                        // this is what the status line has to show.
                        let label = Self::prefetch_request_label(&request);
                        if let Some(progress) = progress.as_ref() {
                            let _ = progress.send(PrefetchProgress::Started {
                                label: label.clone(),
                            });
                            crate::ui_wake::wake_ui();
                        }
                        let sent = match request {
                            PrefetchRequest::Sidecar { raw, prep } => {
                                match load_sidecar_audio(&project_path, &raw) {
                                    Ok((channels, sr, _)) => match prep {
                                        Some(prep) => tx.send(PrefetchResult::Prepared {
                                            raw,
                                            prepared: Box::new(Self::prepare_sidecar(
                                                channels, sr, prep,
                                            )),
                                        }),
                                        None => tx.send(PrefetchResult::Sidecar {
                                            raw,
                                            channels,
                                            sample_rate: sr,
                                        }),
                                    },
                                    Err(_) => Ok(()),
                                }
                            }
                            PrefetchRequest::File { path } => {
                                match crate::audio_io::decode_audio_multi(&path) {
                                    Ok((channels, sr)) => tx.send(PrefetchResult::File {
                                        path,
                                        channels,
                                        sample_rate: sr,
                                    }),
                                    Err(_) => Ok(()),
                                }
                            }
                        };
                        if let Some(progress) = progress.as_ref() {
                            let _ = progress.send(PrefetchProgress::Finished { label });
                            crate::ui_wake::wake_ui();
                        }
                        if sent.is_err() {
                            break;
                        }
                    }
                });
            match handle {
                Ok(handle) => handles.push(handle),
                // Out of threads: the remaining queue is decoded by whoever
                // is already running, or inline below if none started.
                Err(_) => break,
            }
        }
        drop(tx);
        if handles.is_empty() {
            // No worker started at all -- decode inline rather than opening
            // the session with every edit missing.
            let mut queue = queue.lock().unwrap_or_else(|e| e.into_inner());
            while let Some(request) = queue.pop_front() {
                match request {
                    PrefetchRequest::Sidecar { raw, prep } => {
                        if let Ok((channels, sr, _)) = load_sidecar_audio(project_path, &raw) {
                            match prep {
                                Some(prep) => {
                                    prefetch
                                        .prepared
                                        .insert(raw, Self::prepare_sidecar(channels, sr, prep));
                                }
                                None => {
                                    prefetch.sidecars.insert(raw, (channels, sr));
                                }
                            }
                        }
                    }
                    PrefetchRequest::File { path } => {
                        if let Ok((channels, sr)) = crate::audio_io::decode_audio_multi(&path) {
                            prefetch.files.insert(path, (channels, sr));
                        }
                    }
                }
            }
            return prefetch;
        }
        while let Ok(result) = rx.recv() {
            match result {
                PrefetchResult::Sidecar {
                    raw,
                    channels,
                    sample_rate,
                } => {
                    prefetch.sidecars.insert(raw, (channels, sample_rate));
                }
                PrefetchResult::Prepared { raw, prepared } => {
                    prefetch.prepared.insert(raw, *prepared);
                }
                PrefetchResult::File {
                    path,
                    channels,
                    sample_rate,
                } => {
                    prefetch.files.insert(path, (channels, sample_rate));
                }
            }
        }
        for handle in handles {
            let _ = handle.join();
        }
        prefetch
    }

    /// What to call an item in the status line: the file name, which is
    /// what the user recognises, not the raw document reference.
    fn prefetch_request_label(request: &PrefetchRequest) -> String {
        let raw: &str = match request {
            PrefetchRequest::Sidecar { raw, .. } => raw.as_str(),
            PrefetchRequest::File { path } => {
                return path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("(file)")
                    .to_string();
            }
        };
        Path::new(raw)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(raw)
            .to_string()
    }

    /// Normalize a decoded sidecar to the output rate and build its editor
    /// waveform caches. Two O(n) passes over the clip -- on the UI thread,
    /// once per edited tab, this is what a session open still spent its
    /// frame on after the decode itself moved to a worker.
    ///
    /// Pure: no `self`, so it runs wherever the decode does.
    fn prepare_sidecar(
        mut channels: Vec<Vec<f32>>,
        sidecar_sr: u32,
        prep: SidecarPrep,
    ) -> PreparedSidecar {
        let out_sr = prep.out_sr.max(1);
        // A legacy sidecar with no stored rate is assumed to already be at
        // the output rate when its own rate differs -- same rule the UI-side
        // path used, kept so old sessions restore identically.
        let mut buffer_sr = prep.stored_buffer_sr.filter(|v| *v > 0).unwrap_or_else(|| {
            if sidecar_sr.max(1) != out_sr {
                out_sr
            } else {
                sidecar_sr.max(1)
            }
        });
        if buffer_sr != out_sr {
            for ch in channels.iter_mut() {
                *ch = crate::wave::resample_quality(ch, buffer_sr, out_sr, prep.quality);
            }
            buffer_sr = out_sr;
        }
        let samples_len = channels.first().map(|c| c.len()).unwrap_or(0);
        let (waveform_minmax, waveform_pyramid) =
            Self::build_editor_waveform_cache(&channels, samples_len);
        PreparedSidecar {
            channels,
            buffer_sample_rate: buffer_sr,
            samples_len,
            waveform_minmax,
            waveform_pyramid,
        }
    }

    /// Blocking open, kept for the CLI, the kittest harness and unit tests.
    /// The GUI goes through `tick_project_open` instead so the parse and the
    /// heavy restore do not land on one frame.
    pub(super) fn open_project_file(&mut self, path: PathBuf) -> Result<(), String> {
        let parsed = Self::parse_session_document(path)?;
        self.apply_parsed_session(parsed)
    }

    /// Apply a parsed document. `prefetch` holds audio already decoded off
    /// the UI thread; anything missing from it falls back to the inline
    /// decode this function always did, so a caller may pass an empty one.
    pub(super) fn apply_parsed_session(&mut self, parsed: ParsedSession) -> Result<(), String> {
        self.apply_parsed_session_with_audio(parsed, SessionAudioPrefetch::default())
    }

    pub(super) fn apply_parsed_session_with_audio(
        &mut self,
        parsed: ParsedSession,
        mut prefetch: SessionAudioPrefetch,
    ) -> Result<(), String> {
        let ParsedSession {
            path,
            project,
            fingerprint: session_fingerprint,
            comment_free_fingerprint,
            path_repair,
            session_path_mode,
            base_dir,
            file_exists,
            other_exists,
        } = parsed;
        // Every existence question the restore asks was answered on the
        // parse worker; the UI thread must not add a syscall of its own.
        let path_exists = |path: &Path| other_exists.get(path).copied().unwrap_or(true);
        let project = *project;

        let project_path = path.clone();
        self.close_project();
        self.clear_external_data();
        self.project_path = Some(project_path.clone());
        self.session_path_mode = session_path_mode;

        self.search_query = project.app.search_query.clone();
        self.search_use_regex = project.app.search_regex;
        self.auto_play_list_nav = project.app.auto_play_list_nav;
        self.list_columns_window_pos = project
            .app
            .list_columns_window_pos
            .filter(|[x, y]| x.is_finite() && y.is_finite())
            .map(|[x, y]| egui::pos2(x, y))
            .or(self.list_columns_window_global_pos);
        let selected_path = project
            .app
            .selected_path
            .as_deref()
            .map(|raw| resolve_path(raw, &base_dir));
        if let Some(policy) = project.app.export_policy.as_ref() {
            self.export_cfg.save_mode = match policy.save_mode.trim().to_ascii_lowercase().as_str()
            {
                "overwrite" => super::types::SaveMode::Overwrite,
                _ => super::types::SaveMode::NewFile,
            };
            self.export_cfg.conflict = match policy.conflict.trim().to_ascii_lowercase().as_str() {
                "overwrite" => super::types::ConflictPolicy::Overwrite,
                "skip" => super::types::ConflictPolicy::Skip,
                _ => super::types::ConflictPolicy::Rename,
            };
            self.export_cfg.backup_bak = policy.backup_bak;
            self.export_cfg.export_srt = policy.export_srt;
            if !policy.name_template.trim().is_empty() {
                self.export_cfg.name_template = policy.name_template.clone();
            }
            self.export_cfg.dest_folder = policy
                .dest_folder
                .as_deref()
                .map(|raw| resolve_path(raw, &base_dir));
        }
        self.list_columns = super::types::ListColumnConfig {
            edited: project.app.list_columns.edited,
            cover_art: project.app.list_columns.cover_art,
            type_badge: project.app.list_columns.type_badge,
            file: project.app.list_columns.file,
            folder: project.app.list_columns.folder,
            transcript: project.app.list_columns.transcript,
            transcript_language: project.app.list_columns.transcript_language,
            external: project.app.list_columns.external,
            length: project.app.list_columns.length,
            channels: project.app.list_columns.ch,
            sample_rate: project.app.list_columns.sr,
            bits: project.app.list_columns.bits,
            status: project.app.list_columns.status,
            tags: project.app.list_columns.tags,
            bit_rate: project.app.list_columns.bit_rate,
            peak: project.app.list_columns.peak,
            lufs: project.app.list_columns.lufs,
            dbtp: project.app.list_columns.dbtp,
            lufs_s: project.app.list_columns.lufs_s,
            lufs_m: project.app.list_columns.lufs_m,
            bpm: project.app.list_columns.bpm,
            created_at: project.app.list_columns.created_at,
            modified_at: project.app.list_columns.modified_at,
            gain: project.app.list_columns.gain,
            wave: project.app.list_columns.wave,
            comments: project.app.list_columns.comments,
            note: project.app.list_columns.note,
            silence_lead: project.app.list_columns.silence_lead,
            silence_tail: project.app.list_columns.silence_tail,
            edge_zero: project.app.list_columns.edge_zero,
            over_peak: project.app.list_columns.over_peak,
            blank_pad: project.app.list_columns.blank_pad,
        };
        if !project.app.list_columns.order.is_empty() {
            let parsed: Vec<super::types::ColumnKey> = project
                .app
                .list_columns
                .order
                .iter()
                .filter_map(|name| super::types::ColumnKey::parse(name))
                .collect();
            self.list_column_layout = parsed;
        }
        self.list_table_layout_revision = self.list_table_layout_revision.wrapping_add(1);
        for (key, w) in &project.app.list_columns.widths {
            if w.is_finite() && *w > 4.0 {
                self.list_col_widths.insert(key.clone(), *w);
            }
        }
        for column in &mut self.metadata_list_columns {
            column.visible = false;
        }
        let mut available_metadata = std::mem::take(&mut self.metadata_list_columns);
        let mut ordered_metadata = Vec::with_capacity(
            available_metadata
                .len()
                .max(project.app.list_columns.metadata.len()),
        );
        for stored in &project.app.list_columns.metadata {
            let Some(key) = super::types::ColumnKey::parse(&stored.key) else {
                continue;
            };
            if let Some(index) = available_metadata
                .iter()
                .position(|column| column.key == key)
            {
                let mut column = available_metadata.remove(index);
                column.label = stored.label.clone();
                column.visible = stored.visible;
                if stored.width.is_finite() && stored.width >= 10.0 {
                    column.width = stored.width;
                }
                ordered_metadata.push(column);
            } else {
                ordered_metadata.push(super::types::MetadataListColumn {
                    key,
                    label: stored.label.clone(),
                    visible: stored.visible,
                    width: if stored.width.is_finite() && stored.width >= 10.0 {
                        stored.width
                    } else {
                        150.0
                    },
                });
            }
        }
        ordered_metadata.extend(available_metadata);
        self.metadata_list_columns = ordered_metadata;
        self.sanitize_list_column_layout();
        self.sort_key = match project.app.sort_key.as_str() {
            "Folder" => super::types::SortKey::Folder,
            "Transcript" => super::types::SortKey::Transcript,
            "Type" => super::types::SortKey::Type,
            "Length" => super::types::SortKey::Length,
            "Channels" => super::types::SortKey::Channels,
            "SampleRate" => super::types::SortKey::SampleRate,
            "Bits" => super::types::SortKey::Bits,
            "BitRate" => super::types::SortKey::BitRate,
            "Level" => super::types::SortKey::Level,
            "Lufs" => super::types::SortKey::Lufs,
            "TruePeak" => super::types::SortKey::TruePeak,
            "LufsShort" => super::types::SortKey::LufsShort,
            "LufsMomentary" => super::types::SortKey::LufsMomentary,
            "Bpm" => super::types::SortKey::Bpm,
            "SilenceLead" => super::types::SortKey::SilenceLead,
            "SilenceTail" => super::types::SortKey::SilenceTail,
            "EdgeZero" => super::types::SortKey::EdgeZero,
            "OverPeak" => super::types::SortKey::OverPeak,
            "BlankPad" => super::types::SortKey::BlankPad,
            "CreatedAt" => super::types::SortKey::CreatedAt,
            "ModifiedAt" => super::types::SortKey::ModifiedAt,
            "Comments" => super::types::SortKey::Comments,
            value if value.starts_with("normalized:") || value.starts_with("raw:") => self
                .metadata_list_columns
                .iter()
                .position(|column| column.key.serialized_name() == value)
                .map(super::types::SortKey::Metadata)
                .unwrap_or(super::types::SortKey::File),
            _ => super::types::SortKey::File,
        };
        self.sort_dir = match project.app.sort_dir.as_str() {
            "Asc" => super::types::SortDir::Asc,
            "Desc" => super::types::SortDir::Desc,
            _ => super::types::SortDir::None,
        };
        match project.app.theme.as_str() {
            "light" => self.theme_mode = super::types::ThemeMode::Light,
            _ => self.theme_mode = super::types::ThemeMode::Dark,
        }
        if let Some(effect_graph_ui) = project.app.effect_graph_ui.as_ref() {
            self.effect_graph.workspace_open = effect_graph_ui.tab_open;
            self.effect_graph.active_template_id = effect_graph_ui.active_template_id.clone();
        }
        if let Some(cfg) = project.app.transcript_ai_config.clone() {
            self.transcript_ai_cfg = cfg;
            self.sanitize_transcript_ai_config();
            self.refresh_transcript_ai_status();
        }
        self.apply_spectro_config(spectro_config_from_project(&project.spectrogram));

        // The parse worker already statted these; handing the answers to
        // the path-status service saves the list from re-probing every row
        // it draws right after the open.
        self.path_status.clear();
        for (raw, exists) in project.list.files.iter().zip(file_exists.iter()) {
            self.path_status
                .preload(&resolve_path(raw, &base_dir), *exists);
        }
        for (path, exists) in other_exists.iter() {
            self.path_status.preload(path, *exists);
        }
        if !project.list.files.is_empty() {
            self.reset_list_from_project(&project.list.files, &base_dir, &file_exists);
            self.after_add_refresh();
        } else if let Some(root) = project.list.root.as_ref() {
            let root_path = resolve_path(root, &base_dir);
            self.root = Some(root_path);
            self.rescan();
        }
        if !project.list.virtual_items.is_empty() {
            let mut pending = project.list.virtual_items.clone();
            let mut missing_errors: Vec<String> = Vec::new();
            let mut rounds = 0usize;
            while !pending.is_empty() && rounds < 8 {
                rounds += 1;
                let mut next_pending: Vec<ProjectVirtualItem> = Vec::new();
                let mut progressed = false;
                let current_pending = std::mem::take(&mut pending);
                for entry in current_pending.into_iter() {
                    let path = resolve_path(&entry.path, &base_dir);
                    let source = virtual_source_from_project(&entry.source, &base_dir);
                    let op_chain = virtual_ops_from_project(&entry.op_chain);
                    if project.version >= 2 {
                        let manifest = entry
                            .asset_id
                            .as_deref()
                            .and_then(|id| project.assets.iter().find(|asset| asset.id == id));
                        if let Some(manifest) = manifest.filter(|asset| asset.backing == "managed")
                        {
                            let managed_path = resolve_path(&manifest.location, &base_dir);
                            if path_exists(&managed_path) {
                                let mut descriptor =
                                    crate::audio_asset::AudioAssetDescriptor::managed(managed_path);
                                if let Some(id) =
                                    crate::audio_asset::AudioAssetId::from_hex(&manifest.id)
                                {
                                    descriptor.id = id;
                                }
                                descriptor.revision =
                                    crate::audio_asset::AssetRevision(manifest.revision.max(1));
                                descriptor.sample_rate =
                                    manifest.sample_rate.max(descriptor.sample_rate);
                                descriptor.channels = manifest.channels.max(descriptor.channels);
                                descriptor.bits_per_sample =
                                    manifest.bits_per_sample.max(descriptor.bits_per_sample);
                                descriptor.frame_count =
                                    manifest.frame_count.or(descriptor.frame_count);
                                let resident_cache = if descriptor.may_reside_in_memory() {
                                    let asset_path = descriptor
                                        .backing
                                        .file_path()
                                        .unwrap_or(Path::new(""))
                                        .to_path_buf();
                                    prefetch
                                        .take_file(&asset_path)
                                        .map(|(channels, _)| channels)
                                        .or_else(|| {
                                            crate::audio_io::decode_audio_multi(&asset_path)
                                                .ok()
                                                .map(|(channels, _)| channels)
                                        })
                                        .map(|channels| {
                                            std::sync::Arc::new(AudioBuffer::from_channels(
                                                channels,
                                            ))
                                        })
                                } else {
                                    None
                                };
                                let mut item =
                                    if let Some(existing) = self.item_for_path(&path).cloned() {
                                        existing
                                    } else {
                                        self.make_media_item(path.clone())
                                    };
                                item.path = path.clone();
                                item.display_name = if entry.display_name.trim().is_empty() {
                                    item.display_name
                                } else {
                                    entry.display_name.clone()
                                };
                                item.display_folder = std::sync::Arc::from("(virtual)");
                                item.source = MediaSource::Virtual;
                                item.status = super::types::MediaStatus::Ok;
                                item.audio_asset = descriptor;
                                item.meta = resident_cache.as_ref().map(|audio| {
                                    Box::new(super::WavesPreviewer::build_meta_from_audio(
                                        &audio.channels,
                                        manifest.sample_rate.max(1),
                                        manifest.bits_per_sample.max(16),
                                        self.blank_threshold_dbfs,
                                    ))
                                });
                                item.virtual_audio = resident_cache;
                                item.virtual_state = Some(VirtualState {
                                    source: source.clone(),
                                    op_chain: op_chain.clone(),
                                    sample_rate: manifest.sample_rate.max(1),
                                    channels: manifest.channels.max(1),
                                    bits_per_sample: manifest.bits_per_sample.max(16),
                                });
                                if self.item_for_path(&path).is_some() {
                                    if let Some(existing) = self.item_for_path_mut(&path) {
                                        *existing = item;
                                    }
                                } else {
                                    let id = item.id;
                                    self.path_index.insert(path.clone(), id);
                                    self.item_index.insert(id, self.items.len());
                                    self.items.push(item);
                                }
                                progressed = true;
                                continue;
                            }
                        }
                    }
                    let mut channels_opt: Option<Vec<Vec<f32>>> = None;
                    // The op_chain only reconstructs the final audio from a *raw*
                    // source (decoded file / parent virtual). The sidecar, by
                    // contrast, already stores the post-edit audio, so ops must
                    // NOT be re-applied to it. Track where the channels came from.
                    let mut channels_from_raw_source = false;
                    let mut sample_rate = entry.sample_rate.max(1);
                    let bits_per_sample = entry.bits_per_sample.max(16);

                    // 1) Prefer the sidecar snapshot: it is the exact current
                    //    audio of the virtual item, including destructive editor
                    //    edits (gain/fade/normalize/trim) the op_chain cannot
                    //    express. Reconstruct from source only when it's absent.
                    if let Some(raw) = entry.sidecar_audio.as_ref() {
                        match prefetch.take_sidecar(raw).map(Ok).unwrap_or_else(|| {
                            load_sidecar_audio(&project_path, raw)
                                .map(|(channels, sr, _)| (channels, sr))
                        }) {
                            Ok((channels, sr)) => {
                                channels_opt = Some(channels);
                                sample_rate = sr.max(1);
                            }
                            Err(err) => {
                                missing_errors
                                    .push(format!("Virtual sidecar decode failed: {raw} ({err})"));
                            }
                        }
                    }

                    // 2) Fallback: rebuild from the raw source + op_chain.
                    if channels_opt.is_none() {
                        match &source {
                            VirtualSourceRef::FilePath(src_path) => {
                                if path_exists(src_path) {
                                    if let Some((channels, sr)) =
                                        prefetch.take_file(src_path).or_else(|| {
                                            crate::audio_io::decode_audio_multi(src_path).ok()
                                        })
                                    {
                                        channels_opt = Some(channels);
                                        channels_from_raw_source = true;
                                        sample_rate = sr.max(1);
                                    } else {
                                        missing_errors.push(format!(
                                            "Virtual source decode failed: {}",
                                            src_path.display()
                                        ));
                                    }
                                } else {
                                    missing_errors.push(format!(
                                        "Missing virtual source: {}",
                                        src_path.display()
                                    ));
                                }
                            }
                            VirtualSourceRef::VirtualPath(src_path) => {
                                if let Some(src_item) = self.item_for_path(src_path) {
                                    if let Some(audio) = src_item.virtual_audio.as_ref() {
                                        channels_opt = Some((*audio.channels).clone());
                                        channels_from_raw_source = true;
                                        sample_rate = src_item
                                            .virtual_state
                                            .as_ref()
                                            .map(|state| state.sample_rate)
                                            .or_else(|| {
                                                src_item.meta.as_ref().map(|m| m.sample_rate)
                                            })
                                            .filter(|v| *v > 0)
                                            .unwrap_or(sample_rate);
                                    } else {
                                        next_pending.push(entry);
                                        continue;
                                    }
                                } else {
                                    missing_errors.push(format!(
                                        "Missing virtual source item: {}",
                                        src_path.display()
                                    ));
                                }
                            }
                            VirtualSourceRef::Sidecar(_) => {
                                // Sidecar-kind sources keep their tag in source.path
                                // (the sidecar_audio field was already tried above).
                                if let Some(raw) = entry.source.path.as_ref() {
                                    if let Some((channels, sr)) =
                                        prefetch.take_sidecar(raw).or_else(|| {
                                            load_sidecar_audio(&project_path, raw)
                                                .ok()
                                                .map(|(channels, sr, _)| (channels, sr))
                                        })
                                    {
                                        channels_opt = Some(channels);
                                        sample_rate = sr.max(1);
                                    } else {
                                        missing_errors
                                            .push(format!("Virtual sidecar decode failed: {raw}"));
                                    }
                                }
                            }
                        }
                    }
                    let Some(mut channels) = channels_opt else {
                        continue;
                    };
                    if channels_from_raw_source {
                        apply_virtual_ops(&mut channels, &op_chain);
                    }
                    let desired_sr = entry.sample_rate.max(1);
                    if sample_rate != desired_sr {
                        for ch in channels.iter_mut() {
                            *ch = self.resample_mono_with_quality(ch, sample_rate, desired_sr);
                        }
                        sample_rate = desired_sr;
                    }
                    let audio = std::sync::Arc::new(AudioBuffer::from_channels(channels.clone()));
                    let channels_count = channels.len().max(1) as u16;
                    let mut item = if let Some(existing) = self.item_for_path(&path).cloned() {
                        existing
                    } else {
                        self.make_media_item(path.clone())
                    };
                    item.path = path.clone();
                    item.display_name = if entry.display_name.trim().is_empty() {
                        item.display_name
                    } else {
                        entry.display_name.clone()
                    };
                    item.display_folder = std::sync::Arc::from("(virtual)");
                    item.source = MediaSource::Virtual;
                    item.status = super::types::MediaStatus::Ok;
                    item.meta = Some(Box::new(super::WavesPreviewer::build_meta_from_audio(
                        &channels,
                        sample_rate,
                        bits_per_sample,
                        self.blank_threshold_dbfs,
                    )));
                    item.audio_asset = crate::audio_asset::AudioAssetDescriptor::resident(
                        audio.clone(),
                        sample_rate,
                        bits_per_sample,
                    );
                    if let Some(id) = entry
                        .asset_id
                        .as_deref()
                        .and_then(crate::audio_asset::AudioAssetId::from_hex)
                    {
                        item.audio_asset.id = id;
                    }
                    if let Some(revision) = entry.asset_revision {
                        item.audio_asset.revision =
                            crate::audio_asset::AssetRevision(revision.max(1));
                    }
                    item.virtual_audio = Some(audio);
                    item.virtual_state = Some(VirtualState {
                        source: source.clone(),
                        op_chain: op_chain.clone(),
                        sample_rate,
                        channels: channels_count,
                        bits_per_sample,
                    });
                    if self.debug.cfg.enabled {
                        self.debug_log(format!(
                            "virtual restore path={} source_kind={} ops={} sr={} ch={} bits={}",
                            path.display(),
                            match &source {
                                VirtualSourceRef::FilePath(_) => "file",
                                VirtualSourceRef::VirtualPath(_) => "virtual",
                                VirtualSourceRef::Sidecar(_) => "sidecar",
                            },
                            op_chain.len(),
                            sample_rate,
                            channels_count,
                            bits_per_sample
                        ));
                    }
                    if self.item_for_path(&path).is_some() {
                        if let Some(existing) = self.item_for_path_mut(&path) {
                            *existing = item;
                        }
                    } else {
                        let id = item.id;
                        self.path_index.insert(path.clone(), id);
                        self.item_index.insert(id, self.items.len());
                        self.items.push(item);
                    }
                    progressed = true;
                }
                if !progressed {
                    for unresolved in next_pending.into_iter() {
                        missing_errors.push(format!(
                            "Virtual restore unresolved dependency: {}",
                            unresolved.path
                        ));
                    }
                    break;
                }
                pending = next_pending;
            }
            if !pending.is_empty() {
                for entry in pending.into_iter() {
                    let path = resolve_path(&entry.path, &base_dir);
                    if let Some(item) = self.item_for_path_mut(&path) {
                        item.source = MediaSource::Virtual;
                        item.status = super::types::MediaStatus::DecodeFailed(
                            "Virtual restore failed".to_string(),
                        );
                        item.meta = Some(Box::new(missing_file_meta(&path)));
                        item.virtual_state = Some(VirtualState {
                            source: virtual_source_from_project(&entry.source, &base_dir),
                            op_chain: virtual_ops_from_project(&entry.op_chain),
                            sample_rate: entry.sample_rate.max(1),
                            channels: entry.channels.max(1),
                            bits_per_sample: entry.bits_per_sample.max(16),
                        });
                        item.virtual_audio =
                            Some(std::sync::Arc::new(AudioBuffer::from_channels(vec![
                                Vec::new(),
                            ])));
                    }
                }
            }
            if !missing_errors.is_empty() {
                self.debug_log(missing_errors.join("\n"));
            }
            self.rebuild_item_indexes();
            self.refresh_filter_then_sort();
        }

        // Rebind external items to the stable ids/revisions recorded by v2.
        for manifest in &project.assets {
            let item_path = resolve_path(&manifest.item_path, &base_dir);
            let Some(item) = self.item_for_path_mut(&item_path) else {
                continue;
            };
            if item.source == MediaSource::Virtual {
                continue;
            }
            let location = resolve_path(&manifest.location, &base_dir);
            let mut descriptor = if manifest.backing == "managed" {
                crate::audio_asset::AudioAssetDescriptor::managed(location)
            } else {
                crate::audio_asset::AudioAssetDescriptor::external(location)
            };
            if let Some(id) = crate::audio_asset::AudioAssetId::from_hex(&manifest.id) {
                descriptor.id = id;
            }
            descriptor.revision = crate::audio_asset::AssetRevision(manifest.revision.max(1));
            descriptor.sample_rate = manifest.sample_rate.max(descriptor.sample_rate);
            descriptor.channels = manifest.channels.max(descriptor.channels);
            descriptor.bits_per_sample = manifest.bits_per_sample.max(descriptor.bits_per_sample);
            descriptor.frame_count = manifest.frame_count.or(descriptor.frame_count);
            item.audio_asset = descriptor;
        }

        // The rows were built through `make_media_item`, which stamps the
        // default status on everything it makes. Clear that first: a row the
        // user deliberately set back to "no status" saves no assignment, and
        // without this it would come back wearing the default on every open.
        self.clear_all_row_labels();
        for item in project.list.items.iter() {
            let path = resolve_path(&item.path, &base_dir);
            let status = item
                .status
                .as_deref()
                .map(|id| std::sync::Arc::<str>::from(id));
            let tags: Vec<std::sync::Arc<str>> = item
                .tags
                .iter()
                .map(|id| std::sync::Arc::<str>::from(id.as_str()))
                .collect();
            if let Some(list_item) = self.item_for_path_mut(&path) {
                list_item.pending_gain_db = item.pending_gain_db;
                list_item.note = item.note.clone();
                list_item.editor_notes = item.editor_notes.clone();
                list_item.status_id = status;
                list_item.set_tags(tags);
            }
        }
        // Adopt the session's own palettes, so a shared `.nwsess` shows its
        // author's labels and colors rather than this machine's.
        //
        // A session that carries no palette at all -- every session written
        // before statuses existed -- deliberately leaves the current one
        // alone. Replacing it with an empty palette would wipe the set the
        // user built in their preferences, and the very next `save_prefs`
        // would make that permanent.
        if !project.list.statuses.is_empty() {
            self.adopt_palette(
                false,
                crate::app::status_tags::palette_from_project(&project.list.statuses),
            );
            self.default_status = project
                .list
                .default_status
                .as_deref()
                .map(std::sync::Arc::<str>::from);
        }
        if !project.list.tags.is_empty() {
            self.adopt_palette(
                true,
                crate::app::status_tags::palette_from_project(&project.list.tags),
            );
        }
        // Every id the rows use must resolve to something: a session that
        // assigns a label its palette block omitted keeps it rather than
        // losing it on the next save. Also re-interns the row ids.
        self.ensure_label_defs_for_rows();
        for item in project.list.transcript_languages.iter() {
            let path = resolve_path(&item.path, &base_dir);
            self.set_transcript_language_for_path(&path, Some(item.language.clone()));
        }
        for stored in &project.transcripts {
            let path = resolve_path(&stored.item_path, &base_dir);
            if let Some(item) = self.item_for_path_mut(&path) {
                item.transcript = Some(std::sync::Arc::new(stored.document.transcript()));
                item.transcript_language = stored.document.language.clone();
                item.transcript_document = Some(std::sync::Arc::new(stored.document.clone()));
            }
        }
        self.sample_rate_override.clear();
        for override_item in project.list.sample_rate_overrides.iter() {
            if override_item.sample_rate == 0 {
                continue;
            }
            let path = resolve_path(&override_item.path, &base_dir);
            self.sample_rate_override
                .insert(path, override_item.sample_rate);
        }
        self.bit_depth_override.clear();
        for override_item in project.list.bit_depth_overrides.iter() {
            let Some(depth) =
                crate::wave::WavBitDepth::from_project_value(&override_item.bit_depth)
            else {
                continue;
            };
            let path = resolve_path(&override_item.path, &base_dir);
            self.bit_depth_override.insert(path, depth);
        }
        self.format_override.clear();
        for override_item in project.list.format_overrides.iter() {
            let ext = override_item
                .format
                .trim()
                .trim_start_matches('.')
                .to_ascii_lowercase();
            if ext.is_empty() || !crate::audio_io::is_supported_extension(&ext) {
                continue;
            }
            let path = resolve_path(&override_item.path, &base_dir);
            self.format_override.insert(path.clone(), ext);
            self.refresh_display_name_for_path(&path);
        }
        self.external_load_queue.clear();
        self.pending_external_restore = None;
        self.external_load_error = None;
        if let Some(external_state) = project.app.external_state.as_ref() {
            self.external_key_rule = external_key_rule_from_project(&external_state.key_rule);
            self.external_match_input =
                external_match_input_from_project(&external_state.match_input);
            self.external_match_regex = external_state.match_regex.clone();
            self.external_match_replace = external_state.match_replace.clone();
            self.external_scope_regex = external_state.scope_regex.clone();
            self.external_show_unmatched = external_state.show_unmatched;
            self.pending_external_restore = Some(super::PendingExternalRestore {
                active_source: external_state.active_source,
                visible_columns: external_state.visible_columns.clone(),
                key_column: external_state.key_column.clone(),
                show_unmatched: external_state.show_unmatched,
            });
            let mut missing_errors = Vec::new();
            for source in external_state.sources.iter() {
                let source_path = resolve_path(&source.path, &base_dir);
                if path_exists(&source_path) {
                    self.queue_external_load_with_settings(
                        source_path,
                        source.sheet_name.clone(),
                        source.has_header,
                        source.header_row,
                        source.data_row,
                        super::external_ops::ExternalLoadTarget::New,
                    );
                } else {
                    missing_errors.push(format!(
                        "Missing external source: {}",
                        source_path.display()
                    ));
                }
            }
            if !missing_errors.is_empty() {
                self.external_load_error = Some(missing_errors.join("\n"));
            }
            if !self.start_next_external_load_from_queue() {
                self.finalize_pending_external_restore();
            }
        }

        let out_sr = self.audio.shared.out_sample_rate;
        for edit in project.cached_edits.iter() {
            let path = resolve_path(&edit.path, &base_dir);
            // Normalized and cached on the decode worker; the fallback is
            // the old inline path, for a sidecar the prefetch missed.
            let edited = prefetch.take_prepared(&edit.edited_audio).or_else(|| {
                let (chans, sr, _) = load_sidecar_audio(&project_path, &edit.edited_audio).ok()?;
                Some(self.prepare_sidecar_on_ui(
                    &path,
                    chans,
                    sr,
                    edit.buffer_sample_rate,
                    "cached edit",
                ))
            });
            let Some(prepared) = edited else {
                continue;
            };
            let PreparedSidecar {
                channels: chans,
                buffer_sample_rate: buffer_sr,
                samples_len,
                waveform_minmax: waveform,
                waveform_pyramid,
            } = prepared;
            let bits = self.effective_bits_for_path(&path).unwrap_or(32);
            let display_meta = Some(super::WavesPreviewer::build_meta_from_audio(
                &chans,
                buffer_sr,
                bits,
                self.blank_threshold_dbfs,
            ));
            self.edited_cache.insert(
                path,
                super::types::CachedEdit {
                    ch_samples: chans,
                    samples_len,
                    buffer_sample_rate: buffer_sr,
                    waveform_minmax: waveform,
                    waveform_pyramid,
                    display_meta,
                    dirty: edit.dirty,
                    loop_region: edit.loop_region.map(|v| (v[0], v[1])),
                    loop_region_committed: edit.loop_region.map(|v| (v[0], v[1])),
                    loop_region_applied: edit.loop_region.map(|v| (v[0], v[1])),
                    loop_markers_saved: edit.loop_markers_saved.map(|v| (v[0], v[1])),
                    loop_markers_dirty: edit.loop_markers_dirty,
                    markers: edit.markers.iter().map(project_marker_to_entry).collect(),
                    regions: edit.regions.iter().map(project_region_to_entry).collect(),
                    markers_committed: edit.markers.iter().map(project_marker_to_entry).collect(),
                    markers_applied: edit.markers.iter().map(project_marker_to_entry).collect(),
                    markers_saved: edit
                        .markers_saved
                        .iter()
                        .map(project_marker_to_entry)
                        .collect(),
                    markers_dirty: edit.markers_dirty,
                    trim_range: edit.trim_range.map(|v| (v[0], v[1])),
                    loop_xfade_samples: edit.loop_xfade_samples,
                    loop_xfade_shape: loop_shape_from_str(&edit.loop_xfade_shape),
                    fade_in_range: edit.fade_in_range.map(|v| (v[0], v[1])),
                    fade_out_range: edit.fade_out_range.map(|v| (v[0], v[1])),
                    fade_in_shape: fade_shape_from_str(&edit.fade_in_shape),
                    fade_out_shape: fade_shape_from_str(&edit.fade_out_shape),
                    loop_mode: loop_mode_from_str(&edit.loop_mode),
                    tool_state: project_tool_state_to_tool_state(&edit.tool_state),
                    active_tool: tool_kind_from_str(&edit.active_tool),
                    plugin_fx_draft: project_plugin_fx_draft_to_draft(&edit.plugin_fx_draft),
                    plugin_fx_chain: project_plugin_fx_chain_to_draft(
                        &edit.plugin_fx_chain,
                        &edit.plugin_fx_draft,
                    ),
                    show_waveform_overlay: edit.show_waveform_overlay,
                    bpm_enabled: edit.bpm_enabled,
                    bpm_value: edit.bpm_value,
                    bpm_user_set: edit.bpm_user_set,
                    bpm_offset_sec: edit.bpm_offset_sec,
                    time_sig_numerator: edit.time_sig_numerator,
                    time_sig_denominator: edit.time_sig_denominator,
                    extra_selections: vec![],
                    applied_effect_graph: edit.applied_effect_graph.as_ref().map(|stamp| {
                        super::types::AppliedEffectGraphStamp {
                            template_id: stamp.template_id.clone(),
                            template_name: stamp.template_name.clone(),
                            template_updated_at_unix_ms: stamp.template_updated_at_unix_ms,
                        }
                    }),
                },
            );
        }

        for tab in project.tabs.iter() {
            let tab_path = resolve_path(&tab.path, &base_dir);
            let edited = if let Some(raw) = tab.edited_audio.as_ref() {
                prefetch.take_prepared(raw).or_else(|| {
                    let (chans, sr, _) = load_sidecar_audio(&project_path, raw).ok()?;
                    Some(self.prepare_sidecar_on_ui(
                        &tab_path,
                        chans,
                        sr,
                        tab.buffer_sample_rate,
                        "tab edit",
                    ))
                })
            } else {
                None
            };
            if let Some(prepared) = edited {
                let PreparedSidecar {
                    channels: chans,
                    buffer_sample_rate: buffer_sr,
                    samples_len,
                    waveform_minmax: waveform,
                    waveform_pyramid,
                } = prepared;
                let bits = self.effective_bits_for_path(&tab_path).unwrap_or(32);
                let display_meta = Some(super::WavesPreviewer::build_meta_from_audio(
                    &chans,
                    buffer_sr,
                    bits,
                    self.blank_threshold_dbfs,
                ));
                self.edited_cache.insert(
                    tab_path.clone(),
                    super::types::CachedEdit {
                        ch_samples: chans,
                        samples_len,
                        buffer_sample_rate: buffer_sr,
                        waveform_minmax: waveform,
                        waveform_pyramid,
                        display_meta,
                        dirty: tab.dirty,
                        loop_region: tab.loop_region.map(|v| (v[0], v[1])),
                        loop_region_committed: tab.loop_region.map(|v| (v[0], v[1])),
                        loop_region_applied: tab.loop_region.map(|v| (v[0], v[1])),
                        loop_markers_saved: tab.loop_region.map(|v| (v[0], v[1])),
                        loop_markers_dirty: tab.loop_markers_dirty,
                        markers: tab.markers.iter().map(project_marker_to_entry).collect(),
                        regions: tab.regions.iter().map(project_region_to_entry).collect(),
                        markers_committed: tab
                            .markers
                            .iter()
                            .map(project_marker_to_entry)
                            .collect(),
                        markers_applied: tab.markers.iter().map(project_marker_to_entry).collect(),
                        markers_saved: tab.markers.iter().map(project_marker_to_entry).collect(),
                        markers_dirty: tab.markers_dirty,
                        trim_range: tab.trim_range.map(|v| (v[0], v[1])),
                        loop_xfade_samples: tab.loop_xfade_samples,
                        loop_xfade_shape: loop_shape_from_str(&tab.loop_xfade_shape),
                        fade_in_range: tab.fade_in_range.map(|v| (v[0], v[1])),
                        fade_out_range: tab.fade_out_range.map(|v| (v[0], v[1])),
                        fade_in_shape: fade_shape_from_str(&tab.fade_in_shape),
                        fade_out_shape: fade_shape_from_str(&tab.fade_out_shape),
                        loop_mode: loop_mode_from_str(&tab.loop_mode),
                        tool_state: project_tool_state_to_tool_state(&tab.tool_state),
                        active_tool: tool_kind_from_str(&tab.active_tool),
                        plugin_fx_draft: project_plugin_fx_draft_to_draft(&tab.plugin_fx_draft),
                        plugin_fx_chain: project_plugin_fx_chain_to_draft(
                            &tab.plugin_fx_chain,
                            &tab.plugin_fx_draft,
                        ),
                        show_waveform_overlay: tab.show_waveform_overlay,
                        bpm_enabled: tab.bpm_enabled,
                        bpm_value: tab.bpm_value,
                        bpm_user_set: tab.bpm_user_set,
                        bpm_offset_sec: tab.bpm_offset_sec,
                        time_sig_numerator: tab.time_sig_numerator,
                        time_sig_denominator: tab.time_sig_denominator,
                        extra_selections: vec![],
                        applied_effect_graph: None,
                    },
                );
            }
            if !path_exists(&tab_path) {
                let fallback_sr = self.audio.shared.out_sample_rate.max(1);
                if let Some(item) = self.item_for_path_mut(&tab_path) {
                    item.source = MediaSource::Virtual;
                    item.status =
                        super::types::MediaStatus::DecodeFailed(describe_missing(&tab_path));
                    item.meta = Some(Box::new(missing_file_meta(&tab_path)));
                    if item.virtual_state.is_none() {
                        item.virtual_state = Some(VirtualState {
                            source: VirtualSourceRef::FilePath(tab_path.clone()),
                            op_chain: Vec::new(),
                            sample_rate: fallback_sr,
                            channels: 1,
                            bits_per_sample: 32,
                        });
                    }
                    if item.virtual_audio.is_none() {
                        item.virtual_audio =
                            Some(std::sync::Arc::new(AudioBuffer::from_channels(vec![
                                Vec::new(),
                            ])));
                    }
                }
            }
        }

        for tab in project.tabs.iter() {
            let tab_path = resolve_path(&tab.path, &base_dir);
            self.open_or_activate_tab(&tab_path);
            if let Some(idx) = self.tabs.iter().position(|t| t.path == tab_path) {
                let mut preview_overlay = None;
                let mut preview_tool = None;
                if let Some(raw) = tab.preview_audio.as_ref() {
                    if let Some((mut chans, sr)) = prefetch.take_sidecar(raw).or_else(|| {
                        load_sidecar_audio(&project_path, raw)
                            .ok()
                            .map(|(chans, sr, _)| (chans, sr))
                    }) {
                        if sr != out_sr {
                            for ch in chans.iter_mut() {
                                *ch = self.resample_mono_with_quality(ch, sr, out_sr);
                            }
                        }
                        let timeline_len = chans.get(0).map(|c| c.len()).unwrap_or_default();
                        let tool = tab
                            .preview_tool
                            .as_deref()
                            .map(tool_kind_from_str)
                            .unwrap_or(super::types::ToolKind::LoopEdit);
                        preview_overlay =
                            Some(super::WavesPreviewer::preview_overlay_from_channels(
                                chans,
                                tool,
                                timeline_len,
                            ));
                        preview_tool = Some(tool);
                    }
                }
                if let Some(t) = self.tabs.get_mut(idx) {
                    let (primary_view, spec_sub_view, other_sub_view) = primary_view_from_project(
                        tab.primary_view.as_deref(),
                        tab.spec_sub_view.as_deref(),
                        tab.other_sub_view.as_deref(),
                        &tab.view_mode,
                    );
                    t.primary_view = primary_view;
                    t.spec_sub_view = spec_sub_view;
                    t.other_sub_view = other_sub_view;
                    t.metadata_sub_view =
                        metadata_sub_view_from_project(tab.metadata_sub_view.as_deref());
                    t.show_waveform_overlay = tab.show_waveform_overlay;
                    t.channel_view = project_channel_view_to_channel_view(&tab.channel_view);
                    t.active_tool = tool_kind_from_str(&tab.active_tool);
                    t.tool_state = project_tool_state_to_tool_state(&tab.tool_state);
                    t.plugin_fx_draft = project_plugin_fx_draft_to_draft(&tab.plugin_fx_draft);
                    t.plugin_fx_chain = project_plugin_fx_chain_to_draft(
                        &tab.plugin_fx_chain,
                        &tab.plugin_fx_draft,
                    );
                    t.music_analysis_draft = tab
                        .music_analysis
                        .as_ref()
                        .map(|draft| project_music_analysis_to_draft(draft, &base_dir))
                        .unwrap_or_default();
                    t.loop_mode = loop_mode_from_str(&tab.loop_mode);
                    t.loop_region = tab.loop_region.map(|v| (v[0], v[1]));
                    t.loop_xfade_samples = tab.loop_xfade_samples;
                    t.loop_xfade_shape = loop_shape_from_str(&tab.loop_xfade_shape);
                    t.trim_range = tab.trim_range.map(|v| (v[0], v[1]));
                    t.selection = tab.selection.map(|v| (v[0], v[1]));
                    t.editor_note_position_mode = if tab.editor_note_position_mode == "beats" {
                        super::types::EditorNotePositionMode::Beats
                    } else {
                        super::types::EditorNotePositionMode::Time
                    };
                    t.markers = tab.markers.iter().map(project_marker_to_entry).collect();
                    t.markers_saved = t.markers.clone();
                    t.markers_dirty = tab.markers_dirty;
                    t.loop_markers_saved = t.loop_region;
                    t.loop_markers_dirty = tab.loop_markers_dirty;
                    t.fade_in_range = tab.fade_in_range.map(|v| (v[0], v[1]));
                    t.fade_out_range = tab.fade_out_range.map(|v| (v[0], v[1]));
                    t.fade_in_shape = fade_shape_from_str(&tab.fade_in_shape);
                    t.fade_out_shape = fade_shape_from_str(&tab.fade_out_shape);
                    t.bpm_enabled = tab.bpm_enabled;
                    t.bpm_value = tab.bpm_value;
                    t.bpm_user_set = tab.bpm_user_set;
                    t.bpm_offset_sec = tab.bpm_offset_sec;
                    t.time_sig_numerator = tab.time_sig_numerator;
                    t.time_sig_denominator = tab.time_sig_denominator;
                    t.view_offset = tab.view_offset;
                    t.view_offset_exact = tab.view_offset as f64;
                    t.samples_per_px = tab.samples_per_px;
                    t.vertical_zoom = tab.vertical_zoom;
                    t.vertical_view_center = tab.vertical_view_center;
                    t.last_amplitude_nav_rect = None;
                    t.last_amplitude_viewport_rect = None;
                    t.last_amplitude_nav_click_at = 0.0;
                    t.last_amplitude_nav_click_pos = None;
                    Self::invalidate_editor_viewport_cache(t);
                    t.dirty = tab.dirty;
                    if let Some(overlay) = preview_overlay {
                        t.preview_overlay = Some(overlay);
                        t.preview_audio_tool = preview_tool;
                    }
                    if t.samples_len > 0 {
                        Self::editor_clamp_ranges(t);
                    }
                }
            }
        }

        if let Some(active) = project.active_tab {
            if active < self.tabs.len() {
                self.active_tab = Some(active);
            }
        }
        if let Some(active) = self.active_tab {
            let preview = self.tabs.get(active).and_then(|tab| {
                let tool = tab.preview_audio_tool?;
                let overlay = tab.preview_overlay.as_ref()?;
                if !overlay.channels.is_empty() {
                    return Some((tool, overlay.channels.clone(), Vec::new()));
                }
                let mono = overlay.mixdown.as_ref().cloned().unwrap_or_default();
                Some((tool, Vec::new(), mono))
            });
            if let Some((tool, channels, mono)) = preview {
                if !channels.is_empty() {
                    self.set_preview_channels(active, tool, channels);
                } else if !mono.is_empty() {
                    self.set_preview_mono(active, tool, mono);
                }
            }
        }
        if let Some(path) = selected_path {
            if let Some(row) = self.row_for_path(&path) {
                self.selected = Some(row);
                self.selected_multi.clear();
                self.selected_multi.insert(row);
                self.select_anchor = Some(row);
            }
        }
        self.load_effect_graph_library();
        if let Some(template_id) = self.effect_graph.active_template_id.clone() {
            if self.effect_graph_entry_by_id(&template_id).is_some() {
                let _ = self.load_effect_graph_template_into_draft(&template_id);
            } else if self.effect_graph.workspace_open {
                self.push_effect_graph_console(
                    super::types::EffectGraphSeverity::Warning,
                    "session",
                    format!("missing effect graph template: {template_id}"),
                    None,
                );
            }
        }
        if self.effect_graph.workspace_open {
            self.open_effect_graph_workspace();
        } else if self.active_tab.is_some() {
            self.workspace_view = super::types::WorkspaceView::Editor;
        } else {
            self.workspace_view = super::types::WorkspaceView::List;
        }
        // Opening no longer writes the repaired paths back. On a shared
        // file server that made every reader a writer -- each person who
        // merely opened the session rewrote it, non-atomically, racing the
        // people actually saving. The repair stays in memory and rides along
        // to the next real save, which is compare-and-swapped like any other.
        self.session_paths_repaired = path_repair.relocated_references > 0;
        if path_repair.relocated_references > 0 {
            self.debug_log(format!(
                "session paths repaired in memory: {} reference(s) resolved next to {} (written on the next save)",
                path_repair.relocated_references,
                project_path.display()
            ));
        }
        if path_repair.unresolved_references > 0 {
            self.debug_log(format!(
                "session opened with {} unresolved source reference(s)",
                path_repair.unresolved_references
            ));
        }
        self.session_disk_fingerprint = Some(session_fingerprint);
        self.session_comment_free_fingerprint = Some(comment_free_fingerprint);
        self.session_id = project.session_id.clone();
        self.session_revision = project.revision;
        self.session_conflict = None;
        self.session_changed_on_disk = None;
        // Replaced outright rather than merged: this is a different document
        // now, and carrying the previous session's conversation into it would
        // put comments under files it has never heard of.
        self.comments = project.comments.clone();
        super::comments::sort_for_storage(&mut self.comments);
        self.mark_comment_index_dirty();
        self.add_recent_session_path(&project_path);
        self.restart_session_watch();
        // What changed in the *referenced files* since this user last opened
        // this session. The session document itself is covered by the watch
        // above; a colleague replacing a wav never touches it.
        self.begin_session_change_check();
        Ok(())
    }

    pub(super) fn process_ipc_requests(&mut self) {
        let Some(rx) = &self.ipc_rx else {
            return;
        };
        let mut pending: Vec<ipc::IpcRequest> = Vec::new();
        {
            let Ok(rx) = rx.lock() else {
                return;
            };
            while let Ok(req) = rx.try_recv() {
                pending.push(req);
            }
        }
        for mut req in pending {
            if let Some(project) = req.project {
                self.queue_project_open(project);
                continue;
            }
            if let Some(pos) = req.files.iter().position(|p| Self::is_session_path(p)) {
                let session = req.files.remove(pos);
                self.queue_project_open(session);
                continue;
            }
            if !req.files.is_empty() {
                self.start_explicit_file_load(
                    req.files,
                    false,
                    Some(super::types::PendingListLoadTargetKind::OpenEditor),
                    true,
                );
            }
        }
    }

    pub(super) fn handle_dropped_files(&mut self, ctx: &egui::Context) {
        let dropped: Vec<egui::DroppedFile> = ctx.input(|i| i.raw.dropped_files.clone());
        if dropped.is_empty() {
            return;
        }
        // Dropped onto the comments window, the files are being pointed at,
        // not opened. Asked first because everything below this loads them.
        if self.comments_window_absorbs_drop(ctx) {
            return;
        }
        let mut project_path: Option<PathBuf> = None;
        let mut external_path: Option<PathBuf> = None;
        let mut paths: Vec<PathBuf> = Vec::new();
        for f in dropped {
            if let Some(p) = f.path {
                let is_project = Self::is_session_path(&p);
                let is_external = p
                    .extension()
                    .and_then(|s| s.to_str())
                    .map(|s| {
                        let s = s.to_ascii_lowercase();
                        s == "csv" || s == "xlsx" || s == "xls"
                    })
                    .unwrap_or(false);
                if is_project && project_path.is_none() {
                    project_path = Some(p);
                } else if is_external && external_path.is_none() {
                    external_path = Some(p);
                } else if !is_project {
                    if !self.try_restore_virtual_drag_path(&p) {
                        paths.push(p);
                    }
                }
            }
        }
        if let Some(project) = project_path {
            self.queue_project_open(project);
        } else {
            if let Some(data_path) = external_path {
                self.external_sheet_selected = None;
                self.external_sheet_names.clear();
                self.external_settings_dirty = false;
                self.external_load_queue.clear();
                self.pending_external_restore = None;
                self.external_load_error = None;
                self.external_load_target = Some(external_ops::ExternalLoadTarget::New);
                self.show_external_dialog = true;
                self.begin_external_load(data_path);
            }
            if !paths.is_empty() {
                self.start_explicit_file_load(
                    paths,
                    false,
                    Some(super::types::PendingListLoadTargetKind::Select),
                    true,
                );
            }
        }
    }
}
