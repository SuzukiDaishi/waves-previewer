use std::path::PathBuf;

impl super::WavesPreviewer {
    pub(super) fn schedule_lufs_for_path(&mut self, path: PathBuf) {
        use std::time::{Duration, Instant};
        if self.is_virtual_path(&path) {
            return;
        }
        // Debounce repeated edits so we compute LUFS only after changes settle.
        let dl = Instant::now() + Duration::from_millis(400);
        self.lufs_recalc_deadline.insert(path, dl);
    }

    pub(super) fn drain_lufs_recalc_results(&mut self) {
        let mut got_any = false;
        if let Some(rx) = &self.lufs_rx2 {
            while let Ok((p, v)) = rx.try_recv() {
                self.lufs_override.insert(p, v);
                got_any = true;
            }
        }
        if got_any {
            self.lufs_worker_busy = false;
        }
    }

    pub(super) fn pump_lufs_recalc_worker(&mut self) {
        if self.lufs_worker_busy {
            return;
        }
        // Only start one recalculation at a time to keep IO/CPU bounded.
        let now = std::time::Instant::now();
        let Some(path) = self
            .lufs_recalc_deadline
            .iter()
            .find(|(_, dl)| **dl <= now)
            .map(|(p, _)| p.clone())
        else {
            return;
        };
        self.lufs_recalc_deadline.remove(&path);
        let g_db = self.pending_gain_db_for_path(&path);
        if g_db.abs() < 0.0001 {
            self.lufs_override.remove(&path);
            return;
        }
        use std::sync::mpsc;
        let (tx, rx) = mpsc::channel();
        self.lufs_rx2 = Some(rx);
        self.lufs_worker_busy = true;
        std::thread::spawn(move || {
            let res = (|| -> anyhow::Result<f32> {
                let (mut chans, sr) = crate::wave::decode_wav_multi(&path)?;
                // Apply pending gain before LUFS measurement to reflect effective loudness.
                let gain = 10.0f32.powf(g_db / 20.0);
                for ch in chans.iter_mut() {
                    for v in ch.iter_mut() {
                        *v *= gain;
                    }
                }
                crate::wave::lufs_integrated_from_multi(&chans, sr)
            })();
            let val = match res {
                Ok(v) => v,
                Err(_) => f32::NEG_INFINITY,
            };
            let _ = tx.send((path, val));
        });
    }
}
