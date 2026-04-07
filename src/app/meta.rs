use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use super::transcript;
use super::types::{FileMeta, SampleValueKind, Transcript};
use crate::audio_io;

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
}

fn task_path(task: &MetaTask) -> &PathBuf {
    match task {
        MetaTask::Header(path)
        | MetaTask::HeaderOnly(path)
        | MetaTask::Decode(path)
        | MetaTask::Transcript(path)
        | MetaTask::External(path) => path,
    }
}

struct MetaQueue {
    queue: Mutex<VecDeque<MetaTask>>,
    cv: Condvar,
    stop: AtomicBool,
}

pub struct MetaPool {
    shared: Arc<MetaQueue>,
}

impl MetaPool {
    pub fn enqueue(&self, task: MetaTask) {
        let mut q = self.shared.queue.lock().unwrap();
        q.push_back(task);
        self.shared.cv.notify_one();
    }

    pub fn enqueue_front(&self, task: MetaTask) {
        let mut q = self.shared.queue.lock().unwrap();
        q.push_front(task);
        self.shared.cv.notify_one();
    }

    pub fn promote_path(&self, path: &PathBuf) {
        let mut q = self.shared.queue.lock().unwrap();
        if let Some(pos) = q.iter().position(|task| task_path(task) == path) {
            let task = q.remove(pos).unwrap_or(MetaTask::Header(path.clone()));
            q.push_front(task);
            self.shared.cv.notify_one();
        }
    }
}

impl Drop for MetaPool {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        self.shared.cv.notify_all();
    }
}

fn header_meta(path: &PathBuf) -> Result<FileMeta, FileMeta> {
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
        let silent_thresh = 10.0_f32.powf(-80.0 / 20.0);
        Some(if peak_abs > silent_thresh {
            20.0 * peak_abs.log10()
        } else {
            f32::NEG_INFINITY
        })
    }

    match audio_io::read_audio_info(path) {
        Ok(info) => {
            let (marker_fracs, loop_frac) = read_wave_annotation_fracs(
                path,
                info.sample_rate,
                info.total_frames,
                info.duration_secs,
            );
            Ok(FileMeta {
                channels: info.channels,
                sample_rate: info.sample_rate,
                bits_per_sample: info.bits_per_sample,
                sample_value_kind: map_sample_value_kind(info.sample_value_kind),
                bit_rate_bps: info.bit_rate_bps,
                duration_secs: info.duration_secs,
                total_frames: info.total_frames,
                rms_db: None,
                peak_db: quick_peak_db(path),
                lufs_i: None,
                bpm: audio_io::read_audio_bpm(path),
                created_at: info.created_at,
                modified_at: info.modified_at,
                cover_art: decode_cover_art_thumbnail(path),
                thumb: Vec::new(),
                marker_fracs,
                loop_frac,
                decode_error: None,
            })
        }
        Err(_) => Err(FileMeta {
            channels: 0,
            sample_rate: 0,
            bits_per_sample: 0,
            sample_value_kind: SampleValueKind::Unknown,
            bit_rate_bps: None,
            duration_secs: None,
            total_frames: None,
            rms_db: None,
            peak_db: None,
            lufs_i: None,
            bpm: None,
            created_at: None,
            modified_at: None,
            cover_art: None,
            thumb: Vec::new(),
            marker_fracs: Vec::new(),
            loop_frac: None,
            decode_error: Some("Decode failed".to_string()),
        }),
    }
}

fn decode_full_meta(path: &PathBuf) -> Option<FileMeta> {
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
            -120.0
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
        let silent_thresh = 10.0_f32.powf(-80.0 / 20.0);
        let peak_db = if peak_abs > silent_thresh {
            20.0 * peak_abs.log10()
        } else {
            f32::NEG_INFINITY
        };
        let mut thumb = Vec::new();
        crate::wave::build_minmax(&mut thumb, &mono, 128);
        let lufs_i = crate::wave::lufs_integrated_from_multi(&chans, sr).ok();
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
            channels: ch,
            sample_rate: sr,
            bits_per_sample: bits,
            sample_value_kind,
            bit_rate_bps: info.as_ref().and_then(|i| i.bit_rate_bps),
            duration_secs: Some(length_secs),
            total_frames,
            rms_db: Some(rms_db),
            peak_db: Some(peak_db),
            lufs_i,
            bpm,
            created_at: info.as_ref().and_then(|i| i.created_at),
            modified_at: info.as_ref().and_then(|i| i.modified_at),
            cover_art: decode_cover_art_thumbnail(path),
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
            -120.0
        };
        let mut peak_abs = 0.0f32;
        for &v in &mono {
            let a = v.abs();
            if a > peak_abs {
                peak_abs = a;
            }
        }
        let silent_thresh = 10.0_f32.powf(-80.0 / 20.0);
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
            lufs_i: None,
            bpm,
            created_at: info.as_ref().and_then(|i| i.created_at),
            modified_at: info.as_ref().and_then(|i| i.modified_at),
            cover_art: decode_cover_art_thumbnail(path),
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
    let (tx, rx) = mpsc::channel();
    let shared = Arc::new(MetaQueue {
        queue: Mutex::new(VecDeque::new()),
        cv: Condvar::new(),
        stop: AtomicBool::new(false),
    });
    let worker_count = workers.max(1);
    for _ in 0..worker_count {
        let shared = Arc::clone(&shared);
        let tx = tx.clone();
        std::thread::spawn(move || {
            loop {
                let task_opt = {
                    let mut guard = shared.queue.lock().unwrap();
                    loop {
                        if let Some(p) = guard.pop_front() {
                            break Some(p);
                        }
                        if shared.stop.load(Ordering::Relaxed) {
                            break None;
                        }
                        guard = shared.cv.wait(guard).unwrap();
                    }
                };
                let Some(task) = task_opt else {
                    break;
                };

                let (p, do_header, do_decode) = match task {
                    MetaTask::Header(path) => (path, true, true),
                    MetaTask::HeaderOnly(path) => (path, true, false),
                    MetaTask::Decode(path) => (path, false, true),
                    MetaTask::Transcript(path) => {
                        let transcript_data = transcript::srt_path_for_audio(&path)
                            .and_then(|p| transcript::load_srt(&p));
                        let _ = tx.send(MetaUpdate::Transcript(path, transcript_data));
                        continue;
                    }
                    MetaTask::External(_) => {
                        continue;
                    }
                };

                // Stage 1: quick header-only metadata
                let mut header_meta_opt: Option<FileMeta> = None;
                if do_header {
                    match header_meta(&p) {
                        Ok(meta) => {
                            let _ = tx.send(MetaUpdate::Header {
                                path: p.clone(),
                                meta: meta.clone(),
                                finalized: !do_decode,
                            });
                            header_meta_opt = Some(meta);
                        }
                        Err(err_meta) => {
                            let _ = tx.send(MetaUpdate::Full(p.clone(), err_meta));
                            continue;
                        }
                    }
                }

                if do_decode {
                    // Stage 2: decode and compute RMS/thumbnail/LUFS(I)
                    if let Some(full) = decode_full_meta(&p) {
                        let _ = tx.send(MetaUpdate::Full(p.clone(), full));
                    } else if let Some(mut header_meta) = header_meta_opt {
                        header_meta.decode_error = Some("Decode failed".to_string());
                        header_meta.rms_db = None;
                        header_meta.peak_db = None;
                        header_meta.lufs_i = None;
                        header_meta.thumb.clear();
                        let _ = tx.send(MetaUpdate::Full(p.clone(), header_meta));
                    }
                } else if let Some(header_meta) = header_meta_opt {
                    // Header-only tasks are finalized here intentionally.
                    let _ = tx.send(MetaUpdate::Full(p.clone(), header_meta));
                }
            }
        });
    }
    (MetaPool { shared }, rx)
}

#[cfg(test)]
mod tests {
    use super::read_wave_annotation_fracs;
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

    #[test]
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

    #[test]
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
