use std::path::PathBuf;

use super::types::ListPreviewResult;

impl super::WavesPreviewer {
    pub(super) fn spawn_list_preview_full(&mut self, path: PathBuf) {
        use std::sync::mpsc;
        self.list_preview_job_id = self.list_preview_job_id.wrapping_add(1);
        let job_id = self.list_preview_job_id;
        let out_sr = self.audio.shared.out_sample_rate;
        let target_sr = self.sample_rate_override.get(&path).copied().filter(|v| *v > 0);
        let (tx, rx) = mpsc::channel::<ListPreviewResult>();
        std::thread::spawn(move || {
            let res = (|| -> anyhow::Result<ListPreviewResult> {
                let (mut chans, in_sr) = crate::wave::decode_wav_multi(&path)?;
                // Apply sample-rate override for preview, then resample to output rate.
                if let Some(target) = target_sr {
                    let target = target.max(1);
                    if in_sr != target {
                        for c in chans.iter_mut() {
                            *c = crate::wave::resample_linear(c, in_sr, target);
                        }
                    }
                    if target != out_sr {
                        for c in chans.iter_mut() {
                            *c = crate::wave::resample_linear(c, target, out_sr);
                        }
                    }
                } else if in_sr != out_sr {
                    for c in chans.iter_mut() {
                        *c = crate::wave::resample_linear(c, in_sr, out_sr);
                    }
                }
                Ok(ListPreviewResult {
                    path,
                    channels: chans,
                    job_id,
                })
            })();
            if let Ok(result) = res {
                let _ = tx.send(result);
            }
        });
        self.list_preview_rx = Some(rx);
    }

    pub(super) fn drain_list_preview_results(&mut self) {
        if let Some(rx) = &self.list_preview_rx {
            if let Ok(res) = rx.try_recv() {
                // Ignore stale jobs if a newer request was queued.
                if res.job_id == self.list_preview_job_id {
                    if self.active_tab.is_none() && self.playing_path.as_ref() == Some(&res.path) {
                        let buf = crate::audio::AudioBuffer::from_channels(res.channels);
                        self.audio
                            .replace_samples_keep_pos(std::sync::Arc::new(buf));
                    }
                }
                self.list_preview_rx = None;
            }
        }
    }
}
