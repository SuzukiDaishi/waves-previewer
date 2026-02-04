use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use super::transcript;
use super::types::{FileMeta, Transcript};
use crate::audio_io;

#[derive(Clone, Debug)]
pub enum MetaTask {
    Header(PathBuf),
    Decode(PathBuf),
    Transcript(PathBuf),
    External(PathBuf),
}

#[derive(Clone, Debug)]
pub enum MetaUpdate {
    Header(PathBuf, FileMeta),
    Full(PathBuf, FileMeta),
    Transcript(PathBuf, Option<Transcript>),
}

fn task_path(task: &MetaTask) -> &PathBuf {
    match task {
        MetaTask::Header(path)
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
        Ok(info) => Ok(FileMeta {
            channels: info.channels,
            sample_rate: info.sample_rate,
            bits_per_sample: info.bits_per_sample,
            bit_rate_bps: info.bit_rate_bps,
            duration_secs: info.duration_secs,
            rms_db: None,
            peak_db: quick_peak_db(path),
            lufs_i: None,
            bpm: audio_io::read_audio_bpm(path),
            created_at: info.created_at,
            modified_at: info.modified_at,
            thumb: Vec::new(),
            decode_error: None,
        }),
        Err(_) => Err(FileMeta {
            channels: 0,
            sample_rate: 0,
            bits_per_sample: 0,
            bit_rate_bps: None,
            duration_secs: None,
            rms_db: None,
            peak_db: None,
            lufs_i: None,
            bpm: None,
            created_at: None,
            modified_at: None,
            thumb: Vec::new(),
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
        let length_secs = if sr > 0 {
            mono.len() as f32 / sr as f32
        } else {
            f32::NAN
        };
        return Some(FileMeta {
            channels: ch,
            sample_rate: sr,
            bits_per_sample: bits,
            bit_rate_bps: info.as_ref().and_then(|i| i.bit_rate_bps),
            duration_secs: Some(length_secs),
            rms_db: Some(rms_db),
            peak_db: Some(peak_db),
            lufs_i,
            bpm,
            created_at: info.as_ref().and_then(|i| i.created_at),
            modified_at: info.as_ref().and_then(|i| i.modified_at),
            thumb,
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
        return Some(FileMeta {
            channels: info.as_ref().map(|i| i.channels).unwrap_or(0),
            sample_rate: if sr > 0 {
                sr
            } else {
                info.as_ref().map(|i| i.sample_rate).unwrap_or(0)
            },
            bits_per_sample: info.as_ref().map(|i| i.bits_per_sample).unwrap_or(0),
            bit_rate_bps: info.as_ref().and_then(|i| i.bit_rate_bps),
            duration_secs: info.as_ref().and_then(|i| i.duration_secs),
            rms_db: Some(rms_db),
            peak_db: Some(peak_db),
            lufs_i: None,
            bpm,
            created_at: info.as_ref().and_then(|i| i.created_at),
            modified_at: info.as_ref().and_then(|i| i.modified_at),
            thumb,
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

                let (p, do_header) = match task {
                    MetaTask::Header(path) => (path, true),
                    MetaTask::Decode(path) => (path, false),
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
                            let _ = tx.send(MetaUpdate::Header(p.clone(), meta.clone()));
                            header_meta_opt = Some(meta);
                        }
                        Err(err_meta) => {
                            let _ = tx.send(MetaUpdate::Full(p.clone(), err_meta));
                            continue;
                        }
                    }
                }

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
            }
        });
    }
    (MetaPool { shared }, rx)
}
