use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};

use super::types::FileMeta;


struct MetaQueue {
    queue: Mutex<VecDeque<PathBuf>>,
    cv: Condvar,
    stop: AtomicBool,
}

pub struct MetaPool {
    shared: Arc<MetaQueue>,
}

impl MetaPool {
    pub fn enqueue(&self, path: PathBuf) {
        let mut q = self.shared.queue.lock().unwrap();
        q.push_back(path);
        self.shared.cv.notify_one();
    }

    pub fn enqueue_front(&self, path: PathBuf) {
        let mut q = self.shared.queue.lock().unwrap();
        q.push_front(path);
        self.shared.cv.notify_one();
    }

    pub fn promote(&self, path: &PathBuf) {
        let mut q = self.shared.queue.lock().unwrap();
        if let Some(pos) = q.iter().position(|p| p == path) {
            q.remove(pos);
            q.push_front(path.clone());
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

pub fn spawn_meta_pool(workers: usize) -> (MetaPool, std::sync::mpsc::Receiver<(PathBuf, FileMeta)>) {
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
                let path_opt = {
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
                let Some(p) = path_opt else { break; };

                // Stage 1: quick header-only metadata
                let header_ok = if let Ok(reader) = hound::WavReader::open(&p) {
                    let spec = reader.spec();
                    let _ = tx.send((
                        p.clone(),
                        FileMeta {
                            channels: spec.channels,
                            sample_rate: spec.sample_rate,
                            bits_per_sample: spec.bits_per_sample,
                            duration_secs: None,
                            rms_db: None,
                            peak_db: None,
                            lufs_i: None,
                            thumb: Vec::new(),
                        },
                    ));
                    true
                } else {
                    let _ = tx.send((
                        p.clone(),
                        FileMeta {
                            channels: 0,
                            sample_rate: 0,
                            bits_per_sample: 0,
                            duration_secs: None,
                            rms_db: None,
                            peak_db: None,
                            lufs_i: None,
                            thumb: Vec::new(),
                        },
                    ));
                    false
                };
                if !header_ok {
                    continue;
                }

                // Stage 2: decode and compute RMS/thumbnail/LUFS(I)
                if let Ok((chans, sr)) = crate::wave::decode_wav_multi(&p) {
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
                    let rms_db = if rms > 0.0 { 20.0 * rms.log10() } else { -120.0 };
                    // Peak across channels (per-sample max of abs across all channels)
                    let mut peak_abs = 0.0f32;
                    if len > 0 {
                        for i in 0..len {
                            let mut m = 0.0f32;
                            for ch in &chans {
                                if let Some(&v) = ch.get(i) {
                                    let a = v.abs();
                                    if a > m { m = a; }
                                }
                            }
                            if m > peak_abs { peak_abs = m; }
                        }
                    }
                    let peak_db = if peak_abs > 0.0 { 20.0 * peak_abs.log10() } else { f32::NEG_INFINITY };
                    let mut thumb = Vec::new();
                    crate::wave::build_minmax(&mut thumb, &mono, 128);
                    let lufs_i = crate::wave::lufs_integrated_from_multi(&chans, sr).ok();
                    let (ch, bits) = if let Ok(reader2) = hound::WavReader::open(&p) {
                        let s = reader2.spec();
                        (s.channels, s.bits_per_sample)
                    } else {
                        (chans.len() as u16, 0)
                    };
                    let length_secs = if sr > 0 { mono.len() as f32 / sr as f32 } else { f32::NAN };
                    let _ = tx.send((
                        p,
                        FileMeta {
                            channels: ch,
                            sample_rate: sr,
                            bits_per_sample: bits,
                            duration_secs: Some(length_secs),
                            rms_db: Some(rms_db),
                            peak_db: Some(peak_db),
                            lufs_i,
                            thumb,
                        },
                    ));
                }
            }
        });
    }
    (MetaPool { shared }, rx)
}
