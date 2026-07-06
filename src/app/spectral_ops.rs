//! RX-style spectral selection edits: band-limited mute of a
//! time-frequency selection with click-free resynthesis, and "play only
//! the selection" (optionally band-limited) preview playback.
//!
//! Approach (matching iZotope RX / Adobe Audition behaviour):
//! - The frequency mask runs in the STFT domain (Hann analysis/synthesis
//!   windows, 75% overlap, weighted overlap-add) with raised-cosine
//!   transition bands at the selection's frequency edges so no hard
//!   spectral edge rings ("musical noise").
//! - The time edges are handled at sample accuracy by crossfading the
//!   filtered signal against the original with raised-cosine ramps just
//!   inside the selection, so edits never click at the boundaries.

use realfft::RealFftPlanner;

const SPECTRAL_FFT_SIZE: usize = 2048;
const SPECTRAL_HOP_SIZE: usize = SPECTRAL_FFT_SIZE / 4;

/// Raised-cosine ramp weight: 0 at x=0, 1 at x=1.
#[inline]
fn raised_cosine(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    0.5 - 0.5 * (core::f32::consts::PI * x).cos()
}

/// Time-edge weight for position `i` inside a selection `[0, len)`:
/// ramps 0→1 over `fade_n` samples at the start, 1→0 at the end, 1 in
/// between. `fade_n` is clamped so the two ramps never overlap.
#[inline]
fn selection_edge_weight(i: usize, len: usize, fade_n: usize) -> f32 {
    if len == 0 {
        return 0.0;
    }
    let fade_n = fade_n.min(len / 2);
    if fade_n == 0 {
        return 1.0;
    }
    let mut w = 1.0f32;
    if i < fade_n {
        w = w.min(raised_cosine((i as f32 + 0.5) / fade_n as f32));
    }
    if i + fade_n >= len {
        let from_end = len - 1 - i;
        w = w.min(raised_cosine((from_end as f32 + 0.5) / fade_n as f32));
    }
    w
}

/// Per-bin gain for a band `[lo_hz, hi_hz]` with raised-cosine
/// transition bands of `fade_hz` placed just inside the band edges.
/// `keep_band == true` yields a band-pass gain (1 inside, 0 outside);
/// `false` yields the complementary band-stop gain.
fn band_bin_gains(
    fft_size: usize,
    sr: u32,
    lo_hz: f32,
    hi_hz: f32,
    fade_hz: f32,
    keep_band: bool,
) -> Vec<f32> {
    let bins = fft_size / 2 + 1;
    let hz_per_bin = sr.max(1) as f32 / fft_size as f32;
    let nyquist = sr.max(1) as f32 * 0.5;
    let lo = lo_hz.clamp(0.0, nyquist);
    let hi = hi_hz.clamp(0.0, nyquist);
    let (lo, hi) = if lo <= hi { (lo, hi) } else { (hi, lo) };
    // Keep at least one analysis bin of smoothing so the mask never has a
    // brick-wall edge, but never let the two ramps swallow the whole band.
    let fade = fade_hz.max(hz_per_bin).min((hi - lo) * 0.5).max(0.0);
    let mut gains = vec![0.0f32; bins];
    for (bin, g) in gains.iter_mut().enumerate() {
        let f = bin as f32 * hz_per_bin;
        let mut inside = 0.0f32;
        if f >= lo && f <= hi {
            inside = 1.0;
            if fade > 0.0 {
                if f < lo + fade {
                    inside = inside.min(raised_cosine((f - lo) / fade));
                }
                if f > hi - fade {
                    inside = inside.min(raised_cosine((hi - f) / fade));
                }
            }
        }
        *g = if keep_band { inside } else { 1.0 - inside };
    }
    gains
}

/// Apply a per-bin gain mask to `signal` via STFT → mask → weighted
/// overlap-add ISTFT. Returns a signal of the same length. Uses reflect
/// padding of half a window on both sides so the edges reconstruct
/// exactly like the interior.
fn stft_apply_band_gain(
    signal: &[f32],
    sr: u32,
    lo_hz: f32,
    hi_hz: f32,
    fade_hz: f32,
    keep_band: bool,
) -> Vec<f32> {
    let n = signal.len();
    if n == 0 {
        return Vec::new();
    }
    let win = SPECTRAL_FFT_SIZE;
    let hop = SPECTRAL_HOP_SIZE;
    let gains = band_bin_gains(win, sr, lo_hz, hi_hz, fade_hz, keep_band);

    // Reflect-pad by win/2 (repeat-pad when the signal is shorter than
    // the pad) so every output sample has full analysis-window coverage.
    let pad = win / 2;
    let mut padded = Vec::with_capacity(n + 2 * pad + win);
    for i in 0..pad {
        let idx = (pad - i).min(n.saturating_sub(1));
        padded.push(signal[idx.min(n - 1)]);
    }
    padded.extend_from_slice(signal);
    for i in 0..pad {
        let idx = n.saturating_sub(2 + i);
        padded.push(signal[idx.min(n - 1)]);
    }
    while padded.len() < win {
        padded.push(0.0);
    }

    let frame_count = (padded.len() - win) / hop + 1;
    let window: Vec<f32> = (0..win)
        .map(|i| {
            let x = i as f32 / win as f32;
            0.5 - 0.5 * (2.0 * core::f32::consts::PI * x).cos()
        })
        .collect();

    let mut planner = RealFftPlanner::<f32>::new();
    let rfft = planner.plan_fft_forward(win);
    let irfft = planner.plan_fft_inverse(win);
    let mut spec = rfft.make_output_vec();
    let mut frame = vec![0.0f32; win];
    let mut time_out = vec![0.0f32; win];
    let out_len = padded.len();
    let mut out = vec![0.0f32; out_len];
    let mut norm = vec![0.0f32; out_len];
    let inv_win = 1.0 / win as f32;

    for frame_idx in 0..frame_count {
        let start = frame_idx * hop;
        for i in 0..win {
            frame[i] = padded[start + i] * window[i];
        }
        if rfft.process(&mut frame, &mut spec).is_err() {
            return signal.to_vec();
        }
        for (bin, v) in spec.iter_mut().enumerate() {
            *v *= gains[bin.min(gains.len() - 1)];
        }
        // Enforce a real time-domain signal.
        spec[0].im = 0.0;
        if let Some(last) = spec.last_mut() {
            last.im = 0.0;
        }
        if irfft.process(&mut spec, &mut time_out).is_err() {
            return signal.to_vec();
        }
        for i in 0..win {
            // realfft's inverse is unnormalized: divide by the FFT size.
            out[start + i] += time_out[i] * inv_win * window[i];
            norm[start + i] += window[i] * window[i];
        }
    }
    for i in 0..out_len {
        out[i] /= norm[i].max(1e-8);
    }
    out[pad..pad + n].to_vec()
}

impl crate::app::WavesPreviewer {
    /// Ordered primary selection `[start, end)` in display samples, only
    /// when it is valid against the current buffer.
    fn editor_valid_selection(tab: &crate::app::types::EditorTab) -> Option<(usize, usize)> {
        let (a0, b0) = tab.selection?;
        let (s, e) = if a0 <= b0 { (a0, b0) } else { (b0, a0) };
        if e <= s || e > tab.samples_len || tab.samples_len == 0 {
            return None;
        }
        Some((s, e))
    }

    /// Destructively mute the current selection. With a frequency band
    /// selected this is an RX-style spectral mute: the band is removed via
    /// an STFT band-stop with raised-cosine frequency transitions, and the
    /// result is crossfaded against the original at the selection's time
    /// edges. Without a band it is a full-band mute with the same
    /// click-free time fades.
    pub(super) fn editor_apply_spectral_mute_selection(&mut self, tab_idx: usize) {
        let time_fade_ms = self.spectral_edit_time_fade_ms.max(0.0);
        let freq_fade_hz = self.spectral_edit_freq_fade_hz.max(0.0);
        let undo_state = {
            let Some(tab) = self.tabs.get_mut(tab_idx) else {
                return;
            };
            if tab.loading {
                return;
            }
            let Some((s, e)) = Self::editor_valid_selection(tab) else {
                return;
            };
            let band = tab.freq_selection;
            let sr = tab.buffer_sample_rate.max(1);
            let undo_state = Self::capture_undo_state(tab);
            let sel_len = e - s;
            let fade_n = ((time_fade_ms / 1000.0) * sr as f32).round() as usize;
            let fade_n = fade_n.min(sel_len / 2);
            for ch in tab.ch_samples.iter_mut() {
                // Band-stop the selection (with STFT context margins) or
                // fall back to silence for a full-band mute.
                let filtered: Vec<f32> = if let Some((lo, hi)) = band {
                    let seg_s = s.saturating_sub(SPECTRAL_FFT_SIZE);
                    let seg_e = (e + SPECTRAL_FFT_SIZE).min(ch.len());
                    let processed =
                        stft_apply_band_gain(&ch[seg_s..seg_e], sr, lo, hi, freq_fade_hz, false);
                    processed[(s - seg_s)..(s - seg_s + sel_len)].to_vec()
                } else {
                    vec![0.0f32; sel_len]
                };
                for i in 0..sel_len {
                    let w = selection_edge_weight(i, sel_len, fade_n);
                    ch[s + i] = ch[s + i] * (1.0 - w) + filtered[i] * w;
                }
            }
            tab.dirty = true;
            Self::editor_clamp_ranges(tab);
            undo_state
        };
        self.editor_finish_destructive_apply(tab_idx, undo_state, true);
    }

    /// Play only the current selection: everything outside the selected
    /// time range is silenced, and with a frequency band selected the
    /// audio is band-passed (RX-style "play selection"). The editor's
    /// buffer is restored automatically when playback stops.
    pub(super) fn editor_play_selection(&mut self, tab_idx: usize) {
        let time_fade_ms = self.spectral_edit_time_fade_ms.max(0.0);
        let freq_fade_hz = self.spectral_edit_freq_fade_hz.max(0.0);
        let Some(tab) = self.tabs.get(tab_idx) else {
            return;
        };
        if tab.loading {
            return;
        }
        let Some((s, e)) = Self::editor_valid_selection(tab) else {
            return;
        };
        let band = tab.freq_selection;
        let sr = tab.buffer_sample_rate.max(1);
        let sel_len = e - s;
        // Keep the play edges short and click-free even when the mute
        // fade is set to zero.
        let fade_n = ((time_fade_ms.max(3.0) / 1000.0) * sr as f32).round() as usize;
        let fade_n = fade_n.min(sel_len / 2);
        let mut channels = tab.ch_samples.clone();
        for ch in channels.iter_mut() {
            let filtered: Vec<f32> = if let Some((lo, hi)) = band {
                let seg_s = s.saturating_sub(SPECTRAL_FFT_SIZE);
                let seg_e = (e + SPECTRAL_FFT_SIZE).min(ch.len());
                let processed =
                    stft_apply_band_gain(&ch[seg_s..seg_e], sr, lo, hi, freq_fade_hz, true);
                processed[(s - seg_s)..(s - seg_s + sel_len)].to_vec()
            } else {
                ch[s..e].to_vec()
            };
            for v in ch[..s].iter_mut() {
                *v = 0.0;
            }
            for v in ch[e..].iter_mut() {
                *v = 0.0;
            }
            for i in 0..sel_len {
                let w = selection_edge_weight(i, sel_len, fade_n);
                ch[s + i] = filtered[i] * w;
            }
        }
        // Offline render to the output rate (playback principle: processed
        // audio always plays from a fully rendered buffer).
        let mut render_spec = self.offline_render_spec_for_path(&tab.path);
        render_spec.master_gain_db = 0.0;
        render_spec.file_gain_db = 0.0;
        let rendered = Self::render_channels_offline_with_spec(channels, sr, render_spec, false);
        self.audio.stop();
        self.audio.set_samples_channels(rendered);
        self.playback_mark_buffer_source(
            crate::app::PlaybackSourceKind::ToolPreview,
            self.audio.shared.out_sample_rate.max(1),
        );
        let (start_audio, end_audio, loop_selection) = {
            let Some(tab) = self.tabs.get(tab_idx) else {
                return;
            };
            (
                self.map_display_to_audio_sample(tab, s),
                self.map_display_to_audio_sample(tab, e),
                tab.loop_mode != crate::app::types::LoopMode::Off,
            )
        };
        if loop_selection && end_audio > start_audio {
            self.audio.set_loop_region(start_audio, end_audio);
            self.audio.set_loop_enabled(true);
        } else {
            self.audio.set_loop_enabled(false);
        }
        self.audio.seek_to_sample(start_audio);
        self.audio.play();
        self.editor_play_selection_state = Some((tab_idx, e));
    }

    /// Per-frame follow-up for [`Self::editor_play_selection`]: stop at the
    /// selection end (one-shot) and restore the editor's real buffer once
    /// playback is over or the engine was retargeted elsewhere.
    pub(super) fn poll_editor_play_selection(&mut self, ctx: &egui::Context) {
        let Some((tab_idx, end_display)) = self.editor_play_selection_state else {
            return;
        };
        // Keep polling while the one-shot is in flight, even when nothing
        // else animates, so the buffer restore is not delayed until the
        // next input event.
        ctx.request_repaint_after(std::time::Duration::from_millis(33));
        if !matches!(
            self.playback_session.source,
            crate::app::PlaybackSourceKind::ToolPreview
        ) {
            // Something else replaced the preview buffer; nothing to restore.
            self.editor_play_selection_state = None;
            return;
        }
        let playing = self
            .audio
            .shared
            .playing
            .load(std::sync::atomic::Ordering::Relaxed);
        let loop_on = self
            .audio
            .shared
            .loop_enabled
            .load(std::sync::atomic::Ordering::Relaxed);
        let mut finished = !playing;
        if playing && !loop_on {
            if let Some(tab) = self.tabs.get(tab_idx) {
                let end_audio = self.map_display_to_audio_sample(tab, end_display);
                let pos = self
                    .audio
                    .shared
                    .play_pos
                    .load(std::sync::atomic::Ordering::Relaxed);
                if pos >= end_audio {
                    finished = true;
                }
            } else {
                finished = true;
            }
        }
        if finished {
            self.editor_play_selection_state = None;
            if self.tabs.get(tab_idx).is_some() {
                self.preview_restore_audio_for_tab(tab_idx);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f32, sr: u32, len: usize) -> Vec<f32> {
        (0..len)
            .map(|i| (2.0 * core::f32::consts::PI * freq * i as f32 / sr as f32).sin())
            .collect()
    }

    fn rms(sig: &[f32]) -> f32 {
        if sig.is_empty() {
            return 0.0;
        }
        (sig.iter().map(|v| v * v).sum::<f32>() / sig.len() as f32).sqrt()
    }

    #[test]
    fn band_stop_removes_band_keeps_rest() {
        let sr = 48_000u32;
        let len = sr as usize; // 1 second
        let low = sine(440.0, sr, len);
        let high = sine(5_000.0, sr, len);
        let mixed: Vec<f32> = low.iter().zip(&high).map(|(a, b)| a + b).collect();
        let out = stft_apply_band_gain(&mixed, sr, 4_000.0, 6_000.0, 100.0, false);
        assert_eq!(out.len(), mixed.len());
        // Compare against the pure 440 Hz component away from the edges.
        let mid = len / 4..(3 * len / 4);
        let residual: Vec<f32> = out[mid.clone()]
            .iter()
            .zip(&low[mid])
            .map(|(o, l)| o - l)
            .collect();
        let res_rms = rms(&residual);
        assert!(res_rms < 0.02, "5kHz leak after band-stop: rms {res_rms}");
    }

    #[test]
    fn band_pass_keeps_band_removes_rest() {
        let sr = 48_000u32;
        let len = sr as usize;
        let low = sine(440.0, sr, len);
        let high = sine(5_000.0, sr, len);
        let mixed: Vec<f32> = low.iter().zip(&high).map(|(a, b)| a + b).collect();
        let out = stft_apply_band_gain(&mixed, sr, 4_000.0, 6_000.0, 100.0, true);
        let mid = len / 4..(3 * len / 4);
        let residual: Vec<f32> = out[mid.clone()]
            .iter()
            .zip(&high[mid])
            .map(|(o, h)| o - h)
            .collect();
        let res_rms = rms(&residual);
        assert!(res_rms < 0.02, "440Hz leak after band-pass: rms {res_rms}");
    }

    #[test]
    fn full_gain_mask_is_transparent() {
        let sr = 48_000u32;
        let len = 20_000usize;
        let sig = sine(1_234.5, sr, len);
        // Band-stop over an empty band = identity mask.
        let out = stft_apply_band_gain(&sig, sr, 0.0, 0.0, 0.0, false);
        let mid = len / 8..(7 * len / 8);
        let max_err = out[mid.clone()]
            .iter()
            .zip(&sig[mid])
            .map(|(o, s)| (o - s).abs())
            .fold(0.0f32, f32::max);
        assert!(max_err < 1e-3, "STFT round-trip not transparent: {max_err}");
    }

    #[test]
    fn edge_weight_is_symmetric_and_click_free() {
        let len = 1000;
        let fade = 100;
        assert!(selection_edge_weight(0, len, fade) < 0.01);
        assert!(selection_edge_weight(len - 1, len, fade) < 0.01);
        assert_eq!(selection_edge_weight(len / 2, len, fade), 1.0);
        // Monotonic ramp-in.
        let mut prev = 0.0;
        for i in 0..fade {
            let w = selection_edge_weight(i, len, fade);
            assert!(w >= prev);
            prev = w;
        }
    }
}
