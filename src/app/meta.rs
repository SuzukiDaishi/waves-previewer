use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use super::transcript;
use super::types::{FileMeta, SampleValueKind, Transcript};
use crate::audio_io;
/// An idle metadata worker rechecks whether the pool still wants it this often.
pub(crate) const POOL_IDLE_RECHECK: std::time::Duration = std::time::Duration::from_millis(50);


fn map_sample_value_kind(kind: audio_io::SampleValueKind) -> SampleValueKind {
    match kind {
        audio_io::SampleValueKind::Unknown => SampleValueKind::Unknown,
        audio_io::SampleValueKind::Int => SampleValueKind::Int,
        audio_io::SampleValueKind::Float => SampleValueKind::Float,
    }
}

fn decode_cover_art_thumbnail(path: &PathBuf) -> Option<Arc<egui::ColorImage>> {
    const COVER_ART_THUMB_SIZE: u32 = 40;
    const MAX_ARTWORK_BYTES: usize = 16 * 1024 * 1024;

    let bytes = audio_io::read_embedded_artwork(path)?;
    if bytes.is_empty() || bytes.len() > MAX_ARTWORK_BYTES {
        return None;
    }
    let image = image::load_from_memory(&bytes).ok()?;
    let thumb = image.thumbnail(COVER_ART_THUMB_SIZE, COVER_ART_THUMB_SIZE);
    let rgba = thumb.to_rgba8();
    let width = rgba.width() as usize;
    let height = rgba.height() as usize;
    if width == 0 || height == 0 {
        return None;
    }
    Some(Arc::new(egui::ColorImage::from_rgba_unmultiplied(
        [width, height],
        rgba.as_raw(),
    )))
}

/// The list thumbnail for a source: its embedded cover art, or — for a video
/// with none — its first frame.
///
/// `allow_video_poster` is the pool's budget for the fallback (see
/// [`PosterPermit`]). With it false a video with no cover art simply has no
/// thumbnail and the list draws its type badge, exactly as it does today for
/// an untagged wav.
fn decode_list_thumbnail(
    path: &PathBuf,
    allow_video_poster: bool,
) -> Option<Arc<egui::ColorImage>> {
    const COVER_ART_THUMB_SIZE: u32 = 40;
    if let Some(art) = decode_cover_art_thumbnail(path) {
        return Some(art);
    }
    if !allow_video_poster || !crate::media_kind::is_video_path(path) {
        return None;
    }
    let frame = crate::video::decode_poster_frame(path, COVER_ART_THUMB_SIZE)?;
    Some(frame.image)
}

fn annotation_total_frames(
    total_frames: Option<u64>,
    duration_secs: Option<f32>,
    sample_rate: u32,
) -> Option<u64> {
    if let Some(frames) = total_frames.filter(|frames| *frames > 0) {
        return Some(frames);
    }
    let secs = duration_secs.filter(|secs| secs.is_finite() && *secs > 0.0)?;
    let sr = sample_rate.max(1) as f32;
    Some((secs * sr).round().max(1.0) as u64)
}

fn normalized_frac(sample: u64, total_frames: u64) -> Option<f32> {
    if total_frames == 0 {
        return None;
    }
    Some((sample as f32 / total_frames as f32).clamp(0.0, 1.0))
}

fn read_wave_annotation_fracs(
    path: &Path,
    file_sr: u32,
    total_frames: Option<u64>,
    duration_secs: Option<f32>,
) -> (Vec<f32>, Option<(f32, f32)>) {
    let total_frames = annotation_total_frames(total_frames, duration_secs, file_sr);
    let loop_frac = total_frames.and_then(|frames| {
        let (start, end) = crate::loop_markers::read_loop_markers(path)?;
        let start_frac = normalized_frac(start, frames)?;
        let end_frac = normalized_frac(end, frames)?;
        Some(if start_frac <= end_frac {
            (start_frac, end_frac)
        } else {
            (end_frac, start_frac)
        })
    });
    let marker_fracs = if file_sr > 0 {
        total_frames
            .map(|frames| {
                let mut fracs: Vec<f32> = crate::markers::read_markers(path, file_sr, file_sr)
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|marker| normalized_frac(marker.sample as u64, frames))
                    .filter(|frac| frac.is_finite())
                    .collect();
                fracs.sort_by(|a, b| a.total_cmp(b));
                fracs.dedup_by(|a, b| (*a - *b).abs() <= f32::EPSILON);
                fracs
            })
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    (marker_fracs, loop_frac)
}

#[derive(Clone, Debug)]
pub enum MetaTask {
    Header(PathBuf),
    HeaderOnly(PathBuf),
    Decode(PathBuf),
    Transcript(PathBuf),
    External(PathBuf),
    /// A `(virtual)` row whose audio lives in a file the app manages -- a
    /// recording take, a pasted or dragged-in copy. Reads `file`, reports
    /// under `row`: the row's path is only a label, and nothing is there.
    VirtualFile { row: PathBuf, file: PathBuf },
}

#[derive(Clone, Debug)]
pub enum MetaUpdate {
    Header {
        path: PathBuf,
        meta: FileMeta,
        finalized: bool,
    },
    Full(PathBuf, FileMeta),
    Transcript(PathBuf, Option<Transcript>),
    /// A queued-or-running task was cancelled; the UI must drop the path from
    /// its inflight set so the row can be re-requested later.
    Cancelled(PathBuf),
}

fn task_path(task: &MetaTask) -> &PathBuf {
    match task {
        MetaTask::Header(path)
        | MetaTask::HeaderOnly(path)
        | MetaTask::Decode(path)
        | MetaTask::Transcript(path)
        | MetaTask::External(path)
        | MetaTask::VirtualFile { row: path, .. } => path,
    }
}

/// Pending work as a per-path task map plus FIFO lanes of paths.
/// Every operation (enqueue, promote, cancel, pop) is O(1); stale lane
/// entries (path no longer in `tasks`, or now holding a task of the other
/// kind) are skipped at pop time. The old single-VecDeque design made
/// `promote_path` a linear scan under the lock, which the UI thread paid for
/// every visible row every frame.
///
/// Quick work -- headers, transcripts -- has lanes of its own and is always
/// taken before a full decode, so a row's length and rate never wait behind
/// another row's waveform. A `Header` task is itself split: the worker reads
/// the header, reports it, and puts the full decode back in the queue.
struct QueueInner {
    tasks: HashMap<PathBuf, MetaTask>,
    /// High-priority lane: visible rows, near-selected rows, active tab.
    hi: VecDeque<PathBuf>,
    /// Background lane: prefetch pumps, CSV export top-ups.
    lo: VecDeque<PathBuf>,
    /// The same two lanes for full decodes (`MetaTask::Decode`).
    decode_hi: VecDeque<PathBuf>,
    decode_lo: VecDeque<PathBuf>,
    /// Paths currently sitting in `hi` / `decode_hi` (avoids duplicate pushes
    /// when a row promotes itself every frame while visible).
    promoted: HashSet<PathBuf>,
    decode_promoted: HashSet<PathBuf>,
    /// Cancel flags for tasks a worker has already started.
    running: HashMap<PathBuf, Arc<AtomicBool>>,
}

/// Everything but a full decode: work that takes milliseconds, not seconds.
fn is_quick(task: &MetaTask) -> bool {
    !matches!(task, MetaTask::Decode(_))
}

impl QueueInner {
    fn new() -> Self {
        Self {
            tasks: HashMap::new(),
            hi: VecDeque::new(),
            lo: VecDeque::new(),
            decode_hi: VecDeque::new(),
            decode_lo: VecDeque::new(),
            promoted: HashSet::new(),
            decode_promoted: HashSet::new(),
            running: HashMap::new(),
        }
    }

    fn push_back(&mut self, path: PathBuf, quick: bool) {
        if quick {
            self.lo.push_back(path);
        } else {
            self.decode_lo.push_back(path);
        }
    }

    /// Put `path` at the front of its kind's high-priority lane, unless it
    /// is there already.
    fn push_front(&mut self, path: &PathBuf, quick: bool) -> bool {
        let (set, lane) = if quick {
            (&mut self.promoted, &mut self.hi)
        } else {
            (&mut self.decode_promoted, &mut self.decode_hi)
        };
        if set.insert(path.clone()) {
            lane.push_front(path.clone());
            true
        } else {
            false
        }
    }

    /// The next path to run and whether it came from a high-priority lane:
    /// quick work first, high before low, then full decodes the same way.
    fn pop_next(&mut self) -> Option<(PathBuf, bool)> {
        let wants = |tasks: &HashMap<PathBuf, MetaTask>, p: &PathBuf, quick: bool| {
            tasks.get(p).is_some_and(|task| is_quick(task) == quick)
        };
        while let Some(p) = self.hi.pop_front() {
            self.promoted.remove(&p);
            if wants(&self.tasks, &p, true) {
                return Some((p, true));
            }
        }
        while let Some(p) = self.lo.pop_front() {
            if wants(&self.tasks, &p, true) {
                return Some((p, false));
            }
        }
        while let Some(p) = self.decode_hi.pop_front() {
            self.decode_promoted.remove(&p);
            if wants(&self.tasks, &p, false) {
                return Some((p, true));
            }
        }
        while let Some(p) = self.decode_lo.pop_front() {
            if wants(&self.tasks, &p, false) {
                return Some((p, false));
            }
        }
        None
    }
}

struct MetaQueue {
    inner: Mutex<QueueInner>,
    cv: Condvar,
    stop: AtomicBool,
    paused: AtomicBool,
    active_workers: AtomicUsize,
    /// Blank Pad threshold in dBFS, stored as `f32::to_bits`. Kept out of
    /// `MetaTask` on purpose: the queue dedupes tasks by path, so a payload
    /// carrying the threshold would silently keep whichever copy landed first.
    blank_threshold_bits: AtomicU32,
    /// How many workers may be extracting a first frame from a video at once,
    /// and how many currently are.
    ///
    /// Pulling a keyframe out of an mp4 costs orders of magnitude more than
    /// reading an embedded cover image, so a folder of video files must not
    /// turn the thumbnail pass into a decode farm. Zero disables it entirely,
    /// which is what a two-core machine gets.
    poster_limit: AtomicUsize,
    poster_inflight: AtomicUsize,
}

/// Holds one of [`MetaQueue::poster_limit`] slots for as long as a video
/// first-frame extraction is running.
struct PosterPermit {
    shared: Arc<MetaQueue>,
}

impl PosterPermit {
    fn try_acquire(shared: &Arc<MetaQueue>) -> Option<Self> {
        let limit = shared.poster_limit.load(Ordering::Relaxed);
        if limit == 0 {
            return None;
        }
        loop {
            let current = shared.poster_inflight.load(Ordering::Relaxed);
            if current >= limit {
                return None;
            }
            if shared
                .poster_inflight
                .compare_exchange_weak(current, current + 1, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
            {
                return Some(Self {
                    shared: Arc::clone(shared),
                });
            }
        }
    }
}

impl Drop for PosterPermit {
    fn drop(&mut self) {
        self.shared.poster_inflight.fetch_sub(1, Ordering::AcqRel);
    }
}

pub struct MetaPool {
    shared: Arc<MetaQueue>,
}

impl MetaPool {
    pub fn set_active_workers(&self, workers: usize) {
        self.shared
            .active_workers
            .store(workers.max(1), Ordering::Relaxed);
        self.shared.cv.notify_all();
    }

    /// How many video first-frame extractions may run at once. Zero turns the
    /// fallback off, leaving only embedded cover art.
    pub fn set_video_poster_limit(&self, limit: usize) {
        self.shared.poster_limit.store(limit, Ordering::Relaxed);
    }

    pub fn set_paused(&self, paused: bool) {
        let changed = self.shared.paused.swap(paused, Ordering::Relaxed) != paused;
        if changed && !paused {
            self.shared.cv.notify_all();
        }
    }

    pub fn enqueue(&self, task: MetaTask) {
        let path = task_path(&task).clone();
        let quick = is_quick(&task);
        let mut inner = self.shared.inner.lock().unwrap_or_else(|e| e.into_inner());
        // A task of the other kind sits in the other lanes: this one needs
        // an entry in its own.
        let queued = inner.tasks.insert(path.clone(), task);
        if queued.is_none_or(|prev| is_quick(&prev) != quick) {
            inner.push_back(path, quick);
        }
        self.shared.cv.notify_one();
    }

    pub fn enqueue_front(&self, task: MetaTask) {
        let path = task_path(&task).clone();
        let quick = is_quick(&task);
        let mut inner = self.shared.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.tasks.insert(path.clone(), task);
        inner.push_front(&path, quick);
        self.shared.cv.notify_one();
    }

    pub fn promote_path(&self, path: &PathBuf) {
        let mut inner = self.shared.inner.lock().unwrap_or_else(|e| e.into_inner());
        let Some(quick) = inner.tasks.get(path).map(is_quick) else {
            return;
        };
        if inner.push_front(path, quick) {
            self.shared.cv.notify_one();
        }
    }

    /// Cancel work for a path. Queued tasks are dropped before they start;
    /// a task already running has its cancel flag raised and stops at the
    /// next stage boundary. Returns true if a queued (not yet started) task
    /// was removed — in that case no `MetaUpdate` will arrive and the caller
    /// must clear its own inflight bookkeeping.
    pub fn cancel_path(&self, path: &Path) -> bool {
        let mut inner = self.shared.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.promoted.remove(path);
        inner.decode_promoted.remove(path);
        let removed = inner.tasks.remove(path).is_some();
        if let Some(flag) = inner.running.get(path) {
            flag.store(true, Ordering::Relaxed);
        }
        removed
    }

    /// Threshold future full decodes measure the Blank Pad columns at. Rows
    /// holding a measurement taken at a different threshold detect it as
    /// stale and re-queue themselves.
    pub fn set_blank_threshold_dbfs(&self, dbfs: f32) {
        self.shared
            .blank_threshold_bits
            .store(dbfs.to_bits(), Ordering::Relaxed);
    }
}

impl Drop for MetaPool {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        self.shared.cv.notify_all();
    }
}

/// Peak of the first quarter second, the header stage's estimate of a file's
/// level. `None` when that prefix does not decode.
fn quick_peak_db(path: &PathBuf) -> Option<f32> {
    let (mono, _sr, _truncated, _decode_errors) =
        audio_io::decode_audio_mono_prefix_with_errors(path, 0.25).ok()?;
    let mut peak_abs = 0.0f32;
    for &v in &mono {
        let a = v.abs();
        if a > peak_abs {
            peak_abs = a;
        }
    }
    let silent_thresh = crate::levels::silence_amplitude();
    Some(if peak_abs > silent_thresh {
        20.0 * peak_abs.log10()
    } else {
        f32::NEG_INFINITY
    })
}

/// The whole header stage: what the header says, then the extras.
#[cfg(test)]
fn header_meta(path: &PathBuf) -> Result<FileMeta, FileMeta> {
    let mut meta = basic_header_meta(path)?;
    enrich_header_meta(path, &mut meta);
    Ok(meta)
}

/// What the container's header alone says -- length, rate, channels,
/// depth, dates -- and nothing that decodes. The list shows this the moment
/// it arrives, before the estimated peak, markers, BPM and cover art
/// (`enrich_header_meta`), which together cost far more than the header.
fn basic_header_meta(path: &PathBuf) -> Result<FileMeta, FileMeta> {
    if let Some(meta) = no_audio_video_meta(path, false) {
        return Ok(meta);
    }
    match audio_io::read_audio_info(path) {
        Ok(info) => Ok(FileMeta {
            audio_track_absent: false,
            audio_track_unsupported: false,
            unsupported_audio_codec: None,
            channel_mask: info.channel_mask,
            channels: info.channels,
            sample_rate: info.sample_rate,
            bits_per_sample: info.bits_per_sample,
            sample_value_kind: map_sample_value_kind(info.sample_value_kind),
            bit_rate_bps: info.bit_rate_bps,
            duration_secs: info.duration_secs,
            total_frames: info.total_frames,
            rms_db: None,
            peak_db: None,
            peak_db_estimate: true,
            lufs_i: None,
            lufs_m_max: None,
            lufs_s_max: None,
            true_peak_db: None,
            bpm: None,
            silence_lead_ms: None,
            silence_tail_ms: None,
            edge_abs: None,
            blank_pad: None,
            created_at: info.created_at,
            modified_at: info.modified_at,
            cover_art: None,
            thumb: Vec::new(),
            marker_fracs: Vec::new(),
            loop_frac: None,
            decode_error: None,
        }),
        Err(_) => {
            let track_is_aac = audio_io::is_isobmff_path(path)
                && audio_io::probe_isobmff_aac_audio_track(path).unwrap_or(false);
            Err(FileMeta {
                audio_track_absent: false,
                audio_track_unsupported: false,
                unsupported_audio_codec: None,
                channel_mask: None,
                channels: 0,
                sample_rate: 0,
                bits_per_sample: 0,
                sample_value_kind: SampleValueKind::Unknown,
                bit_rate_bps: None,
                duration_secs: None,
                total_frames: None,
                rms_db: None,
                peak_db: None,
                peak_db_estimate: false,
                lufs_i: None,
                lufs_m_max: None,
                lufs_s_max: None,
                true_peak_db: None,
                bpm: None,
                silence_lead_ms: None,
                silence_tail_ms: None,
                edge_abs: None,
                blank_pad: None,
                created_at: None,
                modified_at: None,
                cover_art: None,
                thumb: Vec::new(),
                marker_fracs: Vec::new(),
                loop_frac: None,
                decode_error: Some(if track_is_aac {
                    "AAC UNSUPPORTED".to_string()
                } else {
                    "Decode failed".to_string()
                }),
            })
        }
    }
}

/// The rest of the header stage, onto what `basic_header_meta` read: the
/// peak of the first quarter second, markers and loop, BPM, cover art.
fn enrich_header_meta(path: &PathBuf, meta: &mut FileMeta) {
    // A video with no audio to read came back whole.
    if meta.audio_track_absent || meta.audio_track_unsupported {
        return;
    }
    // An AAC track is only readable where the OS lends its decoder. Where it is
    // not, nothing is asked to decode the file: the row says `AAC UNSUPPORTED`
    // rather than reporting a damaged file. Where it is, a decode that still
    // fails (an N edition without the Media Feature Pack, a truncated track)
    // lands in the same place, because the reason the user needs is the same.
    let track_is_aac = audio_io::is_isobmff_path(path)
        && audio_io::probe_isobmff_aac_audio_track(path).unwrap_or(false);
    let aac_decodable = !track_is_aac || audio_io::aac_decode_available();
    let peak_db = aac_decodable.then(|| quick_peak_db(path)).flatten();
    let aac_unsupported = track_is_aac && peak_db.is_none();
    let (marker_fracs, loop_frac) = read_wave_annotation_fracs(
        path,
        meta.sample_rate,
        meta.total_frames,
        meta.duration_secs,
    );
    meta.peak_db = peak_db;
    meta.bpm = audio_io::read_audio_bpm(path);
    meta.cover_art = decode_cover_art_thumbnail(path);
    meta.marker_fracs = marker_fracs;
    meta.loop_frac = loop_frac;
    meta.decode_error = aac_unsupported.then(|| "AAC UNSUPPORTED".to_string());
}

fn no_audio_video_meta(path: &PathBuf, allow_video_poster: bool) -> Option<FileMeta> {
    if !crate::media_kind::is_video_path(path) {
        return None;
    }
    // `Some(codec)` when the audio is there but nothing here decodes it.
    let (has_audio_track, unsupported_audio_codec) = if crate::mpegts::is_mpegts_path(path) {
        use crate::audio_mpegts::TsAudioPresence;
        let probe = crate::mpegts::TsProbe::open_head(path).ok()?;
        match crate::audio_mpegts::audio_presence(&probe) {
            TsAudioPresence::Decodable => return None,
            TsAudioPresence::Absent => (false, None),
            TsAudioPresence::Unsupported(codec) => (true, Some(codec)),
        }
    } else {
        let has_audio_track = audio_io::probe_isobmff_audio_track(path).ok()?;
        let unsupported = has_audio_track && audio_io::isobmff_aac_audio_unsupported(path);
        (has_audio_track, unsupported.then_some("AAC"))
    };
    let audio_track_unsupported = unsupported_audio_codec.is_some();
    if has_audio_track && !audio_track_unsupported {
        return None;
    }
    let video = crate::video::probe_video_stream(path).ok()?;
    let file_meta = std::fs::metadata(path).ok();
    Some(FileMeta {
        audio_track_absent: !has_audio_track,
        audio_track_unsupported,
        unsupported_audio_codec,
        channel_mask: None,
        channels: 0,
        sample_rate: 0,
        bits_per_sample: 0,
        sample_value_kind: SampleValueKind::Unknown,
        bit_rate_bps: None,
        duration_secs: (video.duration_secs.is_finite() && video.duration_secs > 0.0)
            .then_some(video.duration_secs as f32),
        total_frames: None,
        rms_db: None,
        peak_db: None,
        peak_db_estimate: false,
        lufs_i: None,
        lufs_m_max: None,
        lufs_s_max: None,
        true_peak_db: None,
        bpm: None,
        silence_lead_ms: None,
        silence_tail_ms: None,
        edge_abs: None,
        blank_pad: None,
        created_at: file_meta.as_ref().and_then(|m| m.created().ok()),
        modified_at: file_meta.as_ref().and_then(|m| m.modified().ok()),
        cover_art: decode_list_thumbnail(path, allow_video_poster),
        thumb: Vec::new(),
        marker_fracs: Vec::new(),
        loop_frac: None,
        decode_error: None,
    })
}

fn decode_full_meta(
    path: &PathBuf,
    blank_threshold_dbfs: f32,
    allow_video_poster: bool,
) -> Option<FileMeta> {
    if let Some(meta) = no_audio_video_meta(path, allow_video_poster) {
        return Some(meta);
    }
    let info = audio_io::read_audio_info(path).ok();
    if let Ok((chans, sr, decode_errors)) = audio_io::decode_audio_multi_with_errors(path) {
        // Mono mixdown for RMS/thumbnail
        let len = chans.get(0).map(|c| c.len()).unwrap_or(0);
        let mut mono = Vec::with_capacity(len);
        if len > 0 {
            for i in 0..len {
                let mut acc = 0.0f32;
                let mut c = 0usize;
                for ch in chans.iter() {
                    if let Some(&v) = ch.get(i) {
                        acc += v;
                        c += 1;
                    }
                }
                mono.push(if c > 0 { acc / (c as f32) } else { 0.0 });
            }
        }
        let mut sum_sq = 0.0f64;
        for &v in &mono {
            sum_sq += (v as f64) * (v as f64);
        }
        let n = mono.len().max(1) as f64;
        let rms = (sum_sq / n).sqrt() as f32;
        let rms_db = if rms > 0.0 {
            20.0 * rms.log10()
        } else {
            crate::levels::NO_SIGNAL_DB
        };
        // Peak across channels (per-sample max of abs across all channels)
        let mut peak_abs = 0.0f32;
        if len > 0 {
            for i in 0..len {
                let mut m = 0.0f32;
                for ch in &chans {
                    if let Some(&v) = ch.get(i) {
                        let a = v.abs();
                        if a > m {
                            m = a;
                        }
                    }
                }
                if m > peak_abs {
                    peak_abs = m;
                }
            }
        }
        let silent_thresh = crate::levels::silence_amplitude();
        let peak_db = if peak_abs > silent_thresh {
            20.0 * peak_abs.log10()
        } else {
            f32::NEG_INFINITY
        };
        let mut thumb = Vec::new();
        crate::wave::build_minmax(&mut thumb, &mono, 128);
        let loudness = crate::wave::loudness_metrics_from_multi(&chans, sr).ok();
        let lufs_i = loudness.map(|l| l.lufs_i);
        // Same threshold the batch inspection defaults to; two linear scans
        // over already-decoded channels, so it's computed unconditionally.
        let (silence_lead_ms, silence_tail_ms) = crate::app::inspection::scan_silence_ms(
            &chans,
            sr,
            crate::app::inspection::DEFAULT_SILENCE_THRESHOLD_DBFS,
        );
        // QA columns: two more linear scans over the same resident samples.
        // A zero-frame decode still has to resolve to *something*, or the row
        // would re-queue itself every frame forever; it passes trivially.
        let edge_abs = Some(crate::app::inspection::scan_edge_samples(&chans).unwrap_or(
            crate::app::types::EdgeSamples {
                first_abs: 0.0,
                last_abs: 0.0,
            },
        ));
        let blank_pad = Some(crate::app::inspection::scan_blank_pad(
            &chans,
            sr,
            blank_threshold_dbfs,
        ));
        let bpm = audio_io::read_audio_bpm(path);
        let (ch, bits) = info
            .as_ref()
            .map(|info| (info.channels, info.bits_per_sample))
            .unwrap_or((chans.len() as u16, 0));
        let sample_value_kind = info
            .as_ref()
            .map(|info| map_sample_value_kind(info.sample_value_kind))
            .unwrap_or(SampleValueKind::Unknown);
        let length_secs = if sr > 0 {
            mono.len() as f32 / sr as f32
        } else {
            f32::NAN
        };
        let total_frames = Some(
            info.as_ref()
                .and_then(|i| i.total_frames)
                .unwrap_or(mono.len() as u64),
        );
        let (marker_fracs, loop_frac) =
            read_wave_annotation_fracs(path, sr, total_frames, Some(length_secs));
        return Some(FileMeta {
            audio_track_absent: false,
            audio_track_unsupported: false,
            unsupported_audio_codec: None,
            channel_mask: info.as_ref().and_then(|i| i.channel_mask),
            channels: ch,
            sample_rate: sr,
            bits_per_sample: bits,
            sample_value_kind,
            bit_rate_bps: info.as_ref().and_then(|i| i.bit_rate_bps),
            duration_secs: Some(length_secs),
            total_frames,
            rms_db: Some(rms_db),
            peak_db: Some(peak_db),
            peak_db_estimate: false,
            lufs_i,
            lufs_m_max: loudness.and_then(|l| l.lufs_m_max),
            lufs_s_max: loudness.and_then(|l| l.lufs_s_max),
            true_peak_db: loudness.and_then(|l| l.true_peak_db),
            bpm,
            silence_lead_ms: Some(silence_lead_ms),
            silence_tail_ms: Some(silence_tail_ms),
            edge_abs,
            blank_pad,
            created_at: info.as_ref().and_then(|i| i.created_at),
            modified_at: info.as_ref().and_then(|i| i.modified_at),
            cover_art: decode_list_thumbnail(path, allow_video_poster),
            thumb,
            marker_fracs,
            loop_frac,
            decode_error: if decode_errors > 0 {
                Some(format!("DecodeError x{decode_errors}"))
            } else {
                None
            },
        });
    }
    if let Ok((mono, sr, _truncated, decode_errors)) =
        audio_io::decode_audio_mono_prefix_with_errors(path, 3.0)
    {
        let mut sum_sq = 0.0f64;
        for &v in &mono {
            sum_sq += (v as f64) * (v as f64);
        }
        let n = mono.len().max(1) as f64;
        let rms = (sum_sq / n).sqrt() as f32;
        let rms_db = if rms > 0.0 {
            20.0 * rms.log10()
        } else {
            crate::levels::NO_SIGNAL_DB
        };
        let mut peak_abs = 0.0f32;
        for &v in &mono {
            let a = v.abs();
            if a > peak_abs {
                peak_abs = a;
            }
        }
        let silent_thresh = crate::levels::silence_amplitude();
        let peak_db = if peak_abs > silent_thresh {
            20.0 * peak_abs.log10()
        } else {
            f32::NEG_INFINITY
        };
        let mut thumb = Vec::new();
        crate::wave::build_minmax(&mut thumb, &mono, 128);
        let bpm = audio_io::read_audio_bpm(path);
        let resolved_sr = if sr > 0 {
            sr
        } else {
            info.as_ref().map(|i| i.sample_rate).unwrap_or(0)
        };
        let total_frames = info.as_ref().and_then(|i| i.total_frames);
        let duration_secs = info.as_ref().and_then(|i| i.duration_secs);
        let (marker_fracs, loop_frac) =
            read_wave_annotation_fracs(path, resolved_sr, total_frames, duration_secs);
        return Some(FileMeta {
            audio_track_absent: false,
            audio_track_unsupported: false,
            unsupported_audio_codec: None,
            channel_mask: info.as_ref().and_then(|i| i.channel_mask),
            channels: info.as_ref().map(|i| i.channels).unwrap_or(0),
            sample_rate: resolved_sr,
            bits_per_sample: info.as_ref().map(|i| i.bits_per_sample).unwrap_or(0),
            sample_value_kind: info
                .as_ref()
                .map(|i| map_sample_value_kind(i.sample_value_kind))
                .unwrap_or(SampleValueKind::Unknown),
            bit_rate_bps: info.as_ref().and_then(|i| i.bit_rate_bps),
            duration_secs,
            total_frames,
            rms_db: Some(rms_db),
            peak_db: Some(peak_db),
            peak_db_estimate: true,
            lufs_i: None,
            lufs_m_max: None,
            lufs_s_max: None,
            true_peak_db: None,
            bpm,
            silence_lead_ms: None,
            silence_tail_ms: None,
            edge_abs: None,
            blank_pad: None,
            created_at: info.as_ref().and_then(|i| i.created_at),
            modified_at: info.as_ref().and_then(|i| i.modified_at),
            cover_art: decode_list_thumbnail(path, allow_video_poster),
            thumb,
            marker_fracs,
            loop_frac,
            decode_error: if decode_errors > 0 {
                Some(format!("DecodeError x{decode_errors} (prefix)"))
            } else {
                Some("Decode failed (prefix)".to_string())
            },
        });
    }
    None
}

pub fn spawn_meta_pool(workers: usize) -> (MetaPool, std::sync::mpsc::Receiver<MetaUpdate>) {
    use std::sync::mpsc;
    let worker_count = workers.max(1);
    let (tx, rx) = mpsc::sync_channel(worker_count.saturating_mul(4).max(8));
    let shared = Arc::new(MetaQueue {
        inner: Mutex::new(QueueInner::new()),
        cv: Condvar::new(),
        stop: AtomicBool::new(false),
        paused: AtomicBool::new(false),
        active_workers: AtomicUsize::new(worker_count),
        blank_threshold_bits: AtomicU32::new(
            crate::app::inspection::DEFAULT_BLANK_THRESHOLD_DBFS.to_bits(),
        ),
        poster_limit: AtomicUsize::new(0),
        poster_inflight: AtomicUsize::new(0),
    });
    for worker_index in 0..worker_count {
        let shared = Arc::clone(&shared);
        let tx = tx.clone();
        std::thread::spawn(move || {
            // Decode workers must never compete with the UI thread for CPU;
            // on a saturated machine this is the difference between a
            // responsive list and frozen buttons.
            crate::app::threading::lower_current_thread_priority();
            loop {
                let popped = {
                    let mut guard = shared.inner.lock().unwrap_or_else(|e| e.into_inner());
                    loop {
                        if shared.stop.load(Ordering::Relaxed) {
                            break None;
                        }
                        if shared.paused.load(Ordering::Relaxed) {
                            guard = shared.cv.wait(guard).unwrap();
                            continue;
                        }
                        if worker_index >= shared.active_workers.load(Ordering::Relaxed) {
                            let (next, _) = shared
                                .cv
                                // Wake now and then to notice the pool being resized.
                                .wait_timeout(guard, crate::app::meta::POOL_IDLE_RECHECK)
                                .unwrap_or_else(|error| error.into_inner());
                            guard = next;
                            continue;
                        }
                        if let Some((p, from_hi)) = guard.pop_next() {
                            let task = guard.tasks.remove(&p).expect("task checked above");
                            let cancel = Arc::new(AtomicBool::new(false));
                            guard.running.insert(p, Arc::clone(&cancel));
                            break Some((task, cancel, from_hi));
                        }
                        guard = shared.cv.wait(guard).unwrap();
                    }
                };
                let Some((task, cancel, from_hi)) = popped else {
                    break;
                };
                let task_path_owned = task_path(&task).clone();
                // Read the threshold once per task so the value written into
                // BlankPadScan is exactly the one the scan used.
                let blank_threshold =
                    f32::from_bits(shared.blank_threshold_bits.load(Ordering::Relaxed));
                let poster_permit = PosterPermit::try_acquire(&shared);
                let follow_up =
                    run_meta_task(task, &cancel, &tx, blank_threshold, poster_permit.is_some());
                drop(poster_permit);
                let mut guard = shared.inner.lock().unwrap_or_else(|e| e.into_inner());
                guard.running.remove(&task_path_owned);
                // The full decode of a header just read goes back in the
                // queue, behind every header still waiting. Under the same
                // lock as `running`, so a cancel lands either on the running
                // task (seen here) or on the queued one (`cancel_path`).
                let Some(next) = follow_up else {
                    continue;
                };
                if cancel.load(Ordering::Relaxed) {
                    drop(guard);
                    let _ = tx.send(MetaUpdate::Cancelled(task_path_owned));
                } else if !guard.tasks.contains_key(&task_path_owned) {
                    guard.tasks.insert(task_path_owned.clone(), next);
                    if from_hi {
                        guard.push_front(&task_path_owned, false);
                    } else {
                        guard.push_back(task_path_owned, false);
                    }
                    shared.cv.notify_one();
                }
            }
        });
    }
    (MetaPool { shared }, rx)
}

/// Run one task, reporting as it goes. Returns the task to queue next for
/// the same path, if any: a `Header` task reports the header and hands its
/// full decode back to the queue, so it waits behind other rows' headers.
fn run_meta_task(
    task: MetaTask,
    cancel: &AtomicBool,
    tx: &std::sync::mpsc::SyncSender<MetaUpdate>,
    blank_threshold_dbfs: f32,
    allow_video_poster: bool,
) -> Option<MetaTask> {
    // `p` is the path the update is reported under, `src` the file read.
    // They differ only for a virtual row, which decodes in the same task:
    // there are few of them, and a `Decode` names one path, not two.
    let (p, src, do_header, do_decode, decode_later) = match task {
        MetaTask::Header(path) => (path.clone(), path, true, true, true),
        MetaTask::HeaderOnly(path) => (path.clone(), path, true, false, false),
        MetaTask::Decode(path) => (path.clone(), path, false, true, false),
        MetaTask::VirtualFile { row, file } => (row, file, true, true, false),
        MetaTask::Transcript(path) => {
            let transcript_data =
                transcript::srt_path_for_audio(&path).and_then(|p| transcript::load_srt(&p));
            let _ = tx.send(MetaUpdate::Transcript(path, transcript_data));
            return None;
        }
        MetaTask::External(_) => {
            return None;
        }
    };

    if cancel.load(Ordering::Relaxed) {
        let _ = tx.send(MetaUpdate::Cancelled(p));
        return None;
    }

    // Stage 1: the header. What it says (length, rate, channels) goes out at
    // once; the extras that cost a partial decode follow.
    let mut header_meta_opt: Option<FileMeta> = None;
    if do_header {
        let mut meta = match basic_header_meta(&src) {
            Ok(meta) => meta,
            Err(err_meta) => {
                let _ = tx.send(MetaUpdate::Full(p.clone(), err_meta));
                return None;
            }
        };
        let _ = tx.send(MetaUpdate::Header {
            path: p.clone(),
            meta: meta.clone(),
            finalized: false,
        });
        if cancel.load(Ordering::Relaxed) {
            let _ = tx.send(MetaUpdate::Cancelled(p));
            return None;
        }
        enrich_header_meta(&src, &mut meta);
        if do_decode {
            let _ = tx.send(MetaUpdate::Header {
                path: p.clone(),
                meta: meta.clone(),
                finalized: false,
            });
        }
        header_meta_opt = Some(meta);
    }

    if do_decode && decode_later {
        return Some(MetaTask::Decode(p));
    }
    if do_decode {
        // Stage boundary: skip the expensive full decode when the task was
        // cancelled while the header stage ran.
        if cancel.load(Ordering::Relaxed) {
            let _ = tx.send(MetaUpdate::Cancelled(p));
            return None;
        }
        // Stage 2: decode and compute RMS/thumbnail/LUFS(I)
        if let Some(full) = decode_full_meta(&src, blank_threshold_dbfs, allow_video_poster) {
            let _ = tx.send(MetaUpdate::Full(p.clone(), full));
        } else if let Some(mut header_meta) = header_meta_opt {
            if header_meta.decode_error.is_none() {
                header_meta.decode_error = Some("Decode failed".to_string());
            }
            header_meta.rms_db = None;
            header_meta.peak_db = None;
            header_meta.lufs_i = None;
            header_meta.lufs_m_max = None;
            header_meta.lufs_s_max = None;
            header_meta.true_peak_db = None;
            header_meta.edge_abs = None;
            header_meta.blank_pad = None;
            header_meta.thumb.clear();
            let _ = tx.send(MetaUpdate::Full(p.clone(), header_meta));
        }
    } else if let Some(header_meta) = header_meta_opt {
        // Header-only tasks are finalized here intentionally.
        let _ = tx.send(MetaUpdate::Full(p.clone(), header_meta));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{
        decode_full_meta, header_meta, read_wave_annotation_fracs, spawn_meta_pool, MetaTask,
    };
    use crate::markers::MarkerEntry;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn make_temp_dir(tag: &str) -> PathBuf {
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let seq = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "neowaves_meta_{tag}_{}_{}_{}",
            std::process::id(),
            now_ms,
            seq
        ));
        std::fs::create_dir_all(&dir).expect("create temp meta dir");
        dir
    }

    fn synth_stereo(sr: u32, secs: f32) -> Vec<Vec<f32>> {
        let frames = ((sr as f32) * secs).max(1.0) as usize;
        let mut left = Vec::with_capacity(frames);
        let mut right = Vec::with_capacity(frames);
        for i in 0..frames {
            let t = i as f32 / sr as f32;
            left.push((t * 220.0 * std::f32::consts::TAU).sin() * 0.30);
            right.push((t * 330.0 * std::f32::consts::TAU).sin() * 0.25);
        }
        vec![left, right]
    }

    fn approx_eq(a: f32, b: f32) -> bool {
        (a - b).abs() <= 0.03
    }

    #[test]
    fn persistent_video_only_fixture_is_no_audio_not_a_decode_error() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("test_samples")
            .join("video")
            .join("video_no_audio_6s_30fps.mp4");
        let header = header_meta(&path).expect("valid video-only header");
        assert!(header.audio_track_absent);
        assert!(header.decode_error.is_none());
        assert!((header.duration_secs.unwrap_or_default() - 6.0).abs() < 0.05);

        let full = decode_full_meta(&path, -60.0, false).expect("terminal no-audio metadata");
        assert!(full.audio_track_absent);
        assert!(full.decode_error.is_none());
        assert!(full.thumb.is_empty(), "no waveform should be invented");
    }

    /// Either the OS lends an AAC decoder and the track behaves like any other
    /// audio, or it does not and the row says so — never a file error.
    #[test]
    fn persistent_aac_video_fixture_follows_this_platforms_aac_support() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("test_samples")
            .join("video")
            .join("video_sync_6s_30fps.mp4");
        let decodable = crate::audio_io::aac_decode_available();
        let header = header_meta(&path).expect("valid AAC video header");
        assert!(!header.audio_track_absent);
        assert_eq!(header.audio_track_unsupported, !decodable);
        assert!(header.decode_error.is_none());
        assert!((header.duration_secs.unwrap_or_default() - 6.0).abs() < 0.05);

        let full = decode_full_meta(&path, -60.0, false).expect("terminal AAC metadata");
        assert_eq!(full.audio_track_unsupported, !decodable);
        assert!(full.decode_error.is_none());
        if decodable {
            assert!(
                !full.thumb.is_empty(),
                "decoded audio should draw a waveform"
            );
        } else {
            assert!(full.thumb.is_empty(), "no waveform should be invented");
        }
    }

    #[test]
    fn meta_pool_pause_keeps_queued_work_off_playback_path() {
        let (pool, rx) = spawn_meta_pool(1);
        pool.set_paused(true);
        pool.enqueue(MetaTask::HeaderOnly(
            std::env::temp_dir().join("neowaves-paused-missing.wav"),
        ));
        assert!(
            rx.recv_timeout(std::time::Duration::from_millis(40))
                .is_err(),
            "paused pool must not begin queued list work"
        );
        pool.set_paused(false);
        assert!(
            rx.recv_timeout(std::time::Duration::from_secs(2)).is_ok(),
            "queued work must resume after playback protection ends"
        );
    }

    /// Every update the pool sends until each of `paths` has its `Full`.
    fn updates_until_full(
        rx: &std::sync::mpsc::Receiver<super::MetaUpdate>,
        paths: &[PathBuf],
    ) -> Vec<super::MetaUpdate> {
        use super::MetaUpdate;
        let mut pending: Vec<PathBuf> = paths.to_vec();
        let mut updates = Vec::new();
        while !pending.is_empty() {
            let update = rx
                .recv_timeout(std::time::Duration::from_secs(20))
                .expect("the pool reports");
            if let MetaUpdate::Full(path, _) = &update {
                pending.retain(|p| p != path);
            }
            updates.push(update);
        }
        updates
    }

    /// A row's length and rate never wait behind another row's waveform:
    /// the pool reads every queued header before any full decode, and a
    /// `Header` task hands its own decode back to the queue.
    #[test]
    fn headers_are_read_before_any_full_decode() {
        use super::MetaUpdate;
        let dir = make_temp_dir("header_first");
        let sr = 48_000;
        let paths: Vec<PathBuf> = (0..4).map(|i| dir.join(format!("f{i}.wav"))).collect();
        for path in &paths {
            crate::wave::export_channels_audio(&synth_stereo(sr, 1.5), sr, path)
                .expect("export wav");
        }
        let (pool, rx) = spawn_meta_pool(1);
        pool.set_paused(true);
        for path in &paths[..3] {
            pool.enqueue(MetaTask::Decode(path.clone()));
        }
        pool.enqueue(MetaTask::Header(paths[3].clone()));
        pool.set_paused(false);
        let updates = updates_until_full(&rx, &paths);
        let first_full = updates
            .iter()
            .position(|u| matches!(u, MetaUpdate::Full(..)))
            .expect("a full decode");
        assert!(
            updates[..first_full].iter().any(|u| matches!(
                u,
                MetaUpdate::Header { path, meta, .. }
                    if *path == paths[3] && meta.duration_secs.is_some() && meta.sample_rate == sr
            )),
            "the header queued last arrives before the decodes queued first: {updates:?}"
        );
        // Its own decode went to the back of the decode lane.
        let fulls: Vec<&PathBuf> = updates
            .iter()
            .filter_map(|u| match u {
                MetaUpdate::Full(path, _) => Some(path),
                _ => None,
            })
            .collect();
        assert_eq!(fulls, paths.iter().collect::<Vec<_>>());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The header goes out the moment it is read; the extras that cost a
    /// partial decode (the estimated peak) follow; the waveform last.
    #[test]
    fn a_header_task_reports_the_header_then_its_extras_then_the_decode() {
        use super::MetaUpdate;
        let dir = make_temp_dir("header_stages");
        let path = dir.join("a.wav");
        let sr = 48_000;
        crate::wave::export_channels_audio(&synth_stereo(sr, 1.5), sr, &path).expect("export wav");
        let (pool, rx) = spawn_meta_pool(1);
        pool.enqueue(MetaTask::Header(path.clone()));
        let updates = updates_until_full(&rx, std::slice::from_ref(&path));
        let metas: Vec<(&'static str, &super::FileMeta)> = updates
            .iter()
            .map(|u| match u {
                MetaUpdate::Header { meta, finalized, .. } => {
                    assert!(!finalized, "the decode is still to come");
                    ("header", meta)
                }
                MetaUpdate::Full(_, meta) => ("full", meta),
                other => panic!("unexpected update: {other:?}"),
            })
            .collect();
        let kinds: Vec<&str> = metas.iter().map(|(kind, _)| *kind).collect();
        assert_eq!(kinds, ["header", "header", "full"]);
        let (basic, enriched, full) = (metas[0].1, metas[1].1, metas[2].1);
        assert!(basic.duration_secs.is_some() && basic.sample_rate == sr && basic.channels == 2);
        assert!(basic.peak_db.is_none(), "nothing decoded yet");
        assert!(enriched.peak_db.is_some(), "the quarter-second estimate");
        assert!(enriched.thumb.is_empty());
        assert!(!full.thumb.is_empty(), "the waveform");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A task replaced by one of the other kind still runs: it is found
    /// through its own kind's lane, not the stale entry in the other.
    #[test]
    fn a_task_replaced_by_the_other_kind_still_runs() {
        let dir = make_temp_dir("kind_change");
        let path = dir.join("a.wav");
        let sr = 48_000;
        crate::wave::export_channels_audio(&synth_stereo(sr, 0.5), sr, &path).expect("export wav");
        let (pool, rx) = spawn_meta_pool(1);
        pool.set_paused(true);
        pool.enqueue(MetaTask::Decode(path.clone()));
        pool.enqueue(MetaTask::Header(path.clone()));
        pool.promote_path(&path);
        pool.set_paused(false);
        let updates = updates_until_full(&rx, std::slice::from_ref(&path));
        assert!(
            matches!(updates.first(), Some(super::MetaUpdate::Header { .. })),
            "the header ran: {updates:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A virtual row's metadata comes from the file behind it, and arrives
    /// under the row's own path -- the only key the list can apply it by.
    #[test]
    fn a_virtual_file_task_reads_the_file_and_reports_the_row() {
        use super::MetaUpdate;
        let dir = make_temp_dir("virtual_file");
        let file = dir.join("take.wav");
        let sr = 48_000;
        crate::wave::export_channels_audio(&synth_stereo(sr, 1.5), sr, &file).expect("export wav");
        let row = PathBuf::from("__virtual__").join("7_take.wav");

        let (pool, rx) = spawn_meta_pool(1);
        pool.enqueue(MetaTask::VirtualFile {
            row: row.clone(),
            file: file.clone(),
        });
        let full = loop {
            match rx
                .recv_timeout(std::time::Duration::from_secs(10))
                .expect("the task reports")
            {
                MetaUpdate::Header { path, .. } => assert_eq!(path, row),
                MetaUpdate::Full(path, meta) => {
                    assert_eq!(path, row);
                    break meta;
                }
                other => panic!("unexpected update: {other:?}"),
            }
        };
        assert_eq!(full.channels, 2);
        assert!(approx_eq(full.duration_secs.unwrap_or_default(), 1.5));
        assert!(full.decode_error.is_none());
        assert!(!full.thumb.is_empty(), "the row gets a waveform");
        assert!(full.lufs_i.is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn write_annotations_and_read(
        path: &Path,
        sr: u32,
        frames: u64,
        require_loop: bool,
    ) -> (Vec<f32>, Option<(f32, f32)>) {
        let markers = vec![
            MarkerEntry {
                label: "M01".to_string(),
                sample: (frames as f32 * 0.10) as usize,
            },
            MarkerEntry {
                label: "M02".to_string(),
                sample: (frames as f32 * 0.80) as usize,
            },
        ];
        crate::markers::write_markers(path, sr, sr, &markers).expect("write markers");
        let loop_write = crate::loop_markers::write_loop_markers(
            path,
            Some(((frames as f32 * 0.25) as u64, (frames as f32 * 0.65) as u64)),
        );
        if require_loop {
            loop_write.expect("write loop markers");
        } else if let Err(err) = loop_write {
            eprintln!("warning: skipping m4a loop tag assertion: {err}");
        }
        let (marker_fracs, loop_frac) =
            read_wave_annotation_fracs(path, sr, Some(frames), Some(frames as f32 / sr as f32));
        (marker_fracs, loop_frac)
    }

    #[test]
    fn read_wave_annotation_fracs_reads_unopened_wav_annotations() {
        let dir = make_temp_dir("wav_annotations");
        let path = dir.join("fixture.wav");
        let sr = 44_100;
        let chans = synth_stereo(sr, 1.0);
        crate::wave::export_channels_audio(&chans, sr, &path).expect("export wav");
        let frames = chans[0].len() as u64;

        let (marker_fracs, loop_frac) = write_annotations_and_read(&path, sr, frames, true);
        assert_eq!(marker_fracs.len(), 2);
        assert!(approx_eq(marker_fracs[0], 0.10));
        assert!(approx_eq(marker_fracs[1], 0.80));
        let loop_frac = loop_frac.expect("loop frac");
        assert!(approx_eq(loop_frac.0, 0.25));
        assert!(approx_eq(loop_frac.1, 0.65));
    }

    // Builds its fixture with the MP3 encoder, so it needs one.
    #[test]
    #[cfg(feature = "mp3_lame")]
    fn read_wave_annotation_fracs_reads_unopened_mp3_annotations() {
        let dir = make_temp_dir("mp3_annotations");
        let path = dir.join("fixture.mp3");
        let sr = 44_100;
        let chans = synth_stereo(sr, 1.0);
        crate::wave::export_channels_audio(&chans, sr, &path).expect("export mp3");
        let frames = chans[0].len() as u64;

        let (marker_fracs, loop_frac) = write_annotations_and_read(&path, sr, frames, true);
        assert_eq!(marker_fracs.len(), 2);
        assert!(approx_eq(marker_fracs[0], 0.10));
        assert!(approx_eq(marker_fracs[1], 0.80));
        let loop_frac = loop_frac.expect("loop frac");
        assert!(approx_eq(loop_frac.0, 0.25));
        assert!(approx_eq(loop_frac.1, 0.65));
    }

    // Builds its fixture with the AAC encoder, so it needs one.
    #[test]
    #[cfg(any())]
    fn read_wave_annotation_fracs_reads_unopened_m4a_annotations() {
        let dir = make_temp_dir("m4a_annotations");
        let path = dir.join("fixture.m4a");
        let sr = 44_100;
        let chans = synth_stereo(sr, 1.0);
        crate::wave::export_channels_audio(&chans, sr, &path).expect("export m4a");
        let frames = chans[0].len() as u64;

        let (marker_fracs, loop_frac) = write_annotations_and_read(&path, sr, frames, false);
        assert_eq!(marker_fracs.len(), 2);
        assert!(approx_eq(marker_fracs[0], 0.10));
        assert!(approx_eq(marker_fracs[1], 0.80));
        if let Some(loop_frac) = loop_frac {
            assert!(approx_eq(loop_frac.0, 0.25));
            assert!(approx_eq(loop_frac.1, 0.65));
        }
    }
}
