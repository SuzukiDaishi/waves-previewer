//! Multi-variation audition: play the selected list rows one after
//! another (round-robin or random) to compare variations of a sound.
//! The audition ends on any explicit stop (Space, selecting another row,
//! Cancel) and advances automatically on each natural playback end.

use std::path::PathBuf;

use crate::app::types::{ToastSeverity, VariationAuditionMode, VariationAuditionState};

impl crate::app::WavesPreviewer {
    pub(super) fn start_variation_audition(&mut self, mode: VariationAuditionMode) {
        let paths = self.selected_paths();
        if paths.len() < 2 {
            self.push_toast(
                ToastSeverity::Warning,
                "Audition: select two or more files first",
            );
            return;
        }
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(1)
            .max(1);
        let mut state = VariationAuditionState {
            paths,
            mode,
            cursor: 0,
            played: 1,
            item_started: false,
            rng: seed,
        };
        if mode == VariationAuditionMode::Random {
            state.cursor = (Self::variation_rng_next(&mut state.rng) as usize) % state.paths.len();
        }
        let first = state.paths[state.cursor].clone();
        self.variation_audition = Some(state);
        if !self.variation_play_path(&first) {
            self.variation_audition = None;
        }
    }

    pub(super) fn cancel_variation_audition(&mut self) {
        self.variation_audition = None;
    }

    fn variation_rng_next(rng: &mut u64) -> u64 {
        *rng = rng
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        *rng >> 33
    }

    /// Next cursor for the audition walk: round-robin cycles in order;
    /// random never repeats the current item (for `len >= 2`).
    pub(crate) fn variation_next_cursor(
        mode: VariationAuditionMode,
        cursor: usize,
        len: usize,
        rng: &mut u64,
    ) -> usize {
        if len == 0 {
            return 0;
        }
        match mode {
            VariationAuditionMode::RoundRobin => (cursor + 1) % len,
            VariationAuditionMode::Random => {
                if len <= 1 {
                    0
                } else {
                    let mut next = (Self::variation_rng_next(rng) as usize) % len;
                    if next == cursor {
                        next = (next + 1) % len;
                    }
                    next
                }
            }
        }
    }

    /// Select the row for `path` and start (or queue) its playback through
    /// the normal list-preview machinery.
    fn variation_play_path(&mut self, path: &PathBuf) -> bool {
        let Some(row) = self.row_for_path(path) else {
            return false;
        };
        self.variation_audition_advancing = true;
        self.select_and_load(row, true);
        if self.force_load_selected_list_preview_for_play() {
            if self.playback_mode_needs_fx_buffer() && !self.spawn_playback_fx_render(true) {
                self.list_play_pending = true;
            } else {
                self.audio.play();
            }
        }
        // Not immediately playable => list_play_pending autoplays on load.
        self.variation_audition_advancing = false;
        true
    }

    /// Per-frame driver: mark the current item as heard while it plays,
    /// and on stop decide between natural end (advance) and user stop
    /// (cancel) by whether the playhead reached the end of the buffer.
    pub(super) fn poll_variation_audition(&mut self, ctx: &egui::Context) {
        if self.variation_audition.is_none() {
            return;
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
        let playing = self
            .audio
            .shared
            .playing
            .load(std::sync::atomic::Ordering::Relaxed);
        if playing {
            if let Some(state) = self.variation_audition.as_mut() {
                state.item_started = true;
            }
            return;
        }
        self.variation_audition_step();
    }

    /// One stopped-playback decision step (separated from the frame poll
    /// so tests can drive it deterministically).
    pub(super) fn variation_audition_step(&mut self) {
        let item_started = self
            .variation_audition
            .as_ref()
            .map(|s| s.item_started)
            .unwrap_or(false);
        if !item_started {
            // Still inside the async load window for the current item.
            return;
        }
        let len = self.audio.current_source_len();
        let pos = self
            .audio
            .shared
            .play_pos
            .load(std::sync::atomic::Ordering::Relaxed);
        if len == 0 || pos + 2 < len {
            // Stopped mid-file: an explicit user stop ends the audition.
            self.variation_audition = None;
            return;
        }
        let next_path = {
            let Some(state) = self.variation_audition.as_mut() else {
                return;
            };
            let next =
                Self::variation_next_cursor(state.mode, state.cursor, state.paths.len(), &mut state.rng);
            state.cursor = next;
            state.played += 1;
            state.item_started = false;
            state.paths[next].clone()
        };
        if !self.variation_play_path(&next_path) {
            self.variation_audition = None;
        }
    }

    #[cfg(feature = "kittest")]
    pub fn test_set_list_multi_selection(&mut self, paths: &[PathBuf]) -> bool {
        let mut rows: Vec<usize> = paths
            .iter()
            .filter_map(|p| self.row_for_path(p))
            .collect();
        if rows.is_empty() {
            return false;
        }
        rows.sort_unstable();
        rows.dedup();
        self.selected_multi.clear();
        for row in &rows {
            self.selected_multi.insert(*row);
        }
        self.selected = Some(rows[0]);
        self.select_anchor = Some(rows[0]);
        true
    }

    #[cfg(feature = "kittest")]
    pub fn test_start_variation_audition(&mut self, random: bool) -> bool {
        let mode = if random {
            VariationAuditionMode::Random
        } else {
            VariationAuditionMode::RoundRobin
        };
        self.start_variation_audition(mode);
        self.variation_audition.is_some()
    }

    #[cfg(feature = "kittest")]
    pub fn test_variation_audition_cursor(&self) -> Option<(usize, usize)> {
        self.variation_audition
            .as_ref()
            .map(|s| (s.cursor, s.played))
    }

    #[cfg(feature = "kittest")]
    pub fn test_variation_simulate_natural_end(&mut self) -> bool {
        if self.variation_audition.is_none() {
            return false;
        }
        if let Some(state) = self.variation_audition.as_mut() {
            state.item_started = true;
        }
        let len = self.audio.current_source_len();
        self.audio
            .shared
            .playing
            .store(false, std::sync::atomic::Ordering::Relaxed);
        self.audio
            .shared
            .play_pos
            .store(len, std::sync::atomic::Ordering::Relaxed);
        self.variation_audition_step();
        self.variation_audition.is_some()
    }

    #[cfg(feature = "kittest")]
    pub fn test_variation_simulate_user_stop(&mut self) -> bool {
        if self.variation_audition.is_none() {
            return false;
        }
        if let Some(state) = self.variation_audition.as_mut() {
            state.item_started = true;
        }
        self.audio
            .shared
            .playing
            .store(false, std::sync::atomic::Ordering::Relaxed);
        self.audio
            .shared
            .play_pos
            .store(0, std::sync::atomic::Ordering::Relaxed);
        self.variation_audition_step();
        self.variation_audition.is_none()
    }

    // ---- "Play Selected Together" (simultaneous mix audition) ------------

    pub(crate) const MIX_AUDITION_MAX_FILES: usize = 16;

    /// Start mixing the selection on a worker; the drain plays the result.
    pub(super) fn start_mix_audition(&mut self) {
        let mut paths = self.selected_paths();
        if paths.len() < 2 {
            self.push_toast(
                ToastSeverity::Warning,
                "Play together: select two or more files first",
            );
            return;
        }
        if self.mix_audition_state.is_some() {
            self.push_toast(ToastSeverity::Info, "A mix audition is already being prepared");
            return;
        }
        if paths.len() > Self::MIX_AUDITION_MAX_FILES {
            self.push_toast(
                ToastSeverity::Info,
                format!(
                    "Play together: mixing the first {} of {} selected files",
                    Self::MIX_AUDITION_MAX_FILES,
                    paths.len()
                ),
            );
            paths.truncate(Self::MIX_AUDITION_MAX_FILES);
        }
        let count = paths.len();
        let out_sr = self.audio.shared.out_sample_rate.max(1);
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut decoded: Vec<Vec<Vec<f32>>> = Vec::with_capacity(paths.len());
            for path in &paths {
                match crate::audio_io::decode_audio_multi(path) {
                    Ok((chans, sr)) => {
                        let chans = if sr != out_sr {
                            crate::wave::resample_channels_quality(
                                &chans,
                                sr,
                                out_sr,
                                crate::wave::ResampleQuality::Fast,
                            )
                        } else {
                            chans
                        };
                        decoded.push(chans);
                    }
                    Err(err) => {
                        let _ = tx.send(Err(format!("{}: {err}", path.display())));
                        return;
                    }
                }
            }
            let _ = tx.send(Ok(Self::mix_buffers(&decoded)));
        });
        self.debug_log(format!("mix audition: decoding {count} files"));
        self.mix_audition_state = Some(crate::app::types::MixAuditionState { rx, count });
    }

    /// Cancel a pending mix: dropping the receiver makes the worker's send
    /// fail, so it exits after the file it is currently decoding.
    pub(super) fn cancel_mix_audition(&mut self) {
        if self.mix_audition_state.take().is_some() {
            self.debug_log("mix audition: cancelled".to_string());
            self.push_toast(ToastSeverity::Info, "Play together cancelled");
        }
    }

    /// Equal-power sum of decoded buffers (channels x samples each) at
    /// 1/sqrt(n): output spans the longest input and the widest channel
    /// count; narrower inputs reuse their last channel (mono fans out).
    pub(crate) fn mix_buffers(inputs: &[Vec<Vec<f32>>]) -> Vec<Vec<f32>> {
        let n = inputs
            .iter()
            .filter(|b| b.iter().any(|c| !c.is_empty()))
            .count();
        if n == 0 {
            return Vec::new();
        }
        let out_ch = inputs.iter().map(|b| b.len()).max().unwrap_or(0);
        let out_len = inputs
            .iter()
            .flat_map(|b| b.iter().map(|c| c.len()))
            .max()
            .unwrap_or(0);
        if out_ch == 0 || out_len == 0 {
            return Vec::new();
        }
        let gain = 1.0 / (n as f32).sqrt();
        let mut out = vec![vec![0.0f32; out_len]; out_ch];
        for buf in inputs {
            if buf.is_empty() {
                continue;
            }
            for (ci, out_c) in out.iter_mut().enumerate() {
                let src = &buf[ci.min(buf.len() - 1)];
                for (o, s) in out_c.iter_mut().zip(src.iter()) {
                    *o += *s * gain;
                }
            }
        }
        out
    }

    /// Per-frame: adopt a finished mix as a one-shot preview buffer.
    pub(super) fn drain_mix_audition(&mut self, ctx: &egui::Context) {
        let Some(state) = &self.mix_audition_state else {
            return;
        };
        match state.rx.try_recv() {
            Ok(Ok(mix)) => {
                let count = state.count;
                self.mix_audition_state = None;
                if mix.is_empty() {
                    self.push_toast(ToastSeverity::Warning, "Play together: nothing to mix");
                    return;
                }
                self.audio.stop();
                self.audio.set_samples_channels(mix);
                self.playback_mark_buffer_source(
                    crate::app::PlaybackSourceKind::ToolPreview,
                    self.audio.shared.out_sample_rate.max(1),
                );
                self.audio.set_loop_enabled(false);
                self.audio.seek_to_sample(0);
                self.audio.play();
                self.debug_log(format!("mix audition: playing {count} files"));
                self.push_toast(
                    ToastSeverity::Info,
                    format!("Playing {count} files together (1/\u{221a}n mix)"),
                );
                ctx.request_repaint();
            }
            Ok(Err(msg)) => {
                self.mix_audition_state = None;
                self.debug_log(format!("mix audition failed: {msg}"));
                self.push_toast(ToastSeverity::Error, format!("Play together failed: {msg}"));
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.mix_audition_state = None;
                self.push_toast(ToastSeverity::Error, "Play together: worker vanished");
            }
        }
    }

    #[cfg(feature = "kittest")]
    pub fn test_start_mix_audition(&mut self) -> bool {
        self.start_mix_audition();
        self.mix_audition_state.is_some()
    }

    #[cfg(feature = "kittest")]
    pub fn test_mix_audition_pending(&self) -> bool {
        self.mix_audition_state.is_some()
    }

    #[cfg(feature = "kittest")]
    pub fn test_audio_buffer_shape(&self) -> Option<(usize, usize)> {
        self.audio
            .shared
            .samples
            .load()
            .as_ref()
            .map(|b| (b.channel_count(), b.len()))
    }
}

#[cfg(test)]
mod tests {
    use crate::app::types::VariationAuditionMode;
    use crate::app::WavesPreviewer;

    #[test]
    fn mix_buffers_sums_at_equal_power_and_fans_out_mono() {
        // Two inputs -> gain 1/sqrt(2). Mono + stereo: output is stereo, the
        // mono input feeds both channels, the shorter input pads with zeros.
        let mono = vec![vec![1.0f32, 1.0]];
        let stereo = vec![vec![1.0f32, 0.0, 0.5], vec![-1.0f32, 0.0, 0.5]];
        let out = WavesPreviewer::mix_buffers(&[mono, stereo]);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].len(), 3);
        let g = 1.0 / 2.0f32.sqrt();
        assert!((out[0][0] - 2.0 * g).abs() < 1e-6);
        assert!((out[1][0] - 0.0).abs() < 1e-6, "mono fans into R: 1 + -1");
        assert!((out[0][1] - g).abs() < 1e-6);
        assert!((out[0][2] - 0.5 * g).abs() < 1e-6, "beyond mono's end only stereo remains");
        assert!(WavesPreviewer::mix_buffers(&[]).is_empty());
        assert!(WavesPreviewer::mix_buffers(&[vec![]]).is_empty());
    }

    #[test]
    fn round_robin_cycles_in_order() {
        let mut rng = 7u64;
        let mut cursor = 0usize;
        let seen: Vec<usize> = (0..6)
            .map(|_| {
                cursor = WavesPreviewer::variation_next_cursor(
                    VariationAuditionMode::RoundRobin,
                    cursor,
                    3,
                    &mut rng,
                );
                cursor
            })
            .collect();
        assert_eq!(seen, vec![1, 2, 0, 1, 2, 0]);
    }

    #[test]
    fn random_never_repeats_current() {
        let mut rng = 42u64;
        let mut cursor = 0usize;
        for _ in 0..200 {
            let next = WavesPreviewer::variation_next_cursor(
                VariationAuditionMode::Random,
                cursor,
                4,
                &mut rng,
            );
            assert_ne!(next, cursor, "random audition repeated the same item");
            assert!(next < 4);
            cursor = next;
        }
    }

    #[test]
    fn degenerate_lengths_are_safe() {
        let mut rng = 1u64;
        assert_eq!(
            WavesPreviewer::variation_next_cursor(VariationAuditionMode::Random, 0, 0, &mut rng),
            0
        );
        assert_eq!(
            WavesPreviewer::variation_next_cursor(VariationAuditionMode::Random, 0, 1, &mut rng),
            0
        );
        assert_eq!(
            WavesPreviewer::variation_next_cursor(
                VariationAuditionMode::RoundRobin,
                0,
                1,
                &mut rng
            ),
            0
        );
    }
}
