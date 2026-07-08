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

/// Image-like frequency warp of the STFT (liquify-style): each warp point
/// pushes spectrogram content near (`sample`, `freq_hz`) by `delta_hz` along
/// the frequency axis, with Gaussian falloff in time (`time_sigma` samples)
/// and frequency (`freq_sigma` Hz).
///
/// The remap is a backward warp evaluated at the destination: for each output
/// bin, the displacement kernel is centered on each point's *target*
/// frequency (`freq_hz + delta_hz`), so grabbed content lands where it was
/// dragged. Complex bins are linearly interpolated along frequency, and a
/// per-bin cumulative phase rotation of `2*pi*shift*hop/sr` per frame keeps
/// shifted partials phase-coherent across frames (phase-vocoder style).
///
/// `points` carries sample positions relative to the start of `signal`.
/// Returns a signal of the same length.
fn stft_warp_frequency(
    signal: &[f32],
    sr: u32,
    points: &[(f32, f32, f32)], // (sample_pos, freq_hz, delta_hz)
    time_sigma: f32,
    freq_sigma: f32,
) -> Vec<f32> {
    let n = signal.len();
    if n == 0 || points.is_empty() {
        return signal.to_vec();
    }
    let win = SPECTRAL_FFT_SIZE;
    let hop = SPECTRAL_HOP_SIZE;
    let bins = win / 2 + 1;
    let hz_per_bin = sr.max(1) as f32 / win as f32;
    let time_sigma = time_sigma.max(1.0);
    let freq_sigma = freq_sigma.max(hz_per_bin);
    let time_cutoff = time_sigma * 3.0;

    // Reflect-pad by win/2 (same scheme as stft_apply_band_gain) so every
    // output sample has full analysis-window coverage. With pad == win/2 the
    // center of analysis frame `k` sits at signal position `k * hop`.
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
    let mut warped = rfft.make_output_vec();
    let mut frame = vec![0.0f32; win];
    let mut time_out = vec![0.0f32; win];
    let out_len = padded.len();
    let mut out = vec![0.0f32; out_len];
    let mut norm = vec![0.0f32; out_len];
    let inv_win = 1.0 / win as f32;
    // Per-bin cumulative phase rotation for phase-coherent shifting. Once a
    // bin has accumulated rotation it keeps it (a constant all-pass) so the
    // rotation never jumps when a warp region ends.
    let mut cum_phase = vec![0.0f32; bins];
    let mut any_phase = false;
    let phase_per_shift = 2.0 * core::f32::consts::PI * hop as f32 / sr.max(1) as f32;

    for frame_idx in 0..frame_count {
        let start = frame_idx * hop;
        let t_center = (frame_idx * hop) as f32; // signal coords (pad == win/2)
        for i in 0..win {
            frame[i] = padded[start + i] * window[i];
        }
        if rfft.process(&mut frame, &mut spec).is_err() {
            return signal.to_vec();
        }

        // Time weights of the active points for this frame.
        let active: Vec<(f32, f32)> = points // (target_hz, weighted_delta)
            .iter()
            .filter_map(|&(p_t, p_f, p_d)| {
                if p_d == 0.0 {
                    return None;
                }
                let dt = t_center - p_t;
                if dt.abs() > time_cutoff {
                    return None;
                }
                let wt = (-0.5 * (dt / time_sigma) * (dt / time_sigma)).exp();
                Some((p_f + p_d, p_d * wt))
            })
            .collect();

        if active.is_empty() && !any_phase {
            // Identity frame: overlap-add the analysis frame unchanged.
            if irfft.process(&mut spec, &mut time_out).is_err() {
                return signal.to_vec();
            }
            for i in 0..win {
                out[start + i] += time_out[i] * inv_win * window[i];
                norm[start + i] += window[i] * window[i];
            }
            continue;
        }

        for bin in 0..bins {
            let f_out = bin as f32 * hz_per_bin;
            let mut shift = 0.0f32;
            for &(target_hz, wdelta) in &active {
                let df = (f_out - target_hz) / freq_sigma;
                shift += wdelta * (-0.5 * df * df).exp();
            }
            let src_pos = (f_out - shift) / hz_per_bin;
            let mut v = if src_pos >= 0.0 && src_pos <= (bins - 1) as f32 {
                let i0 = src_pos.floor() as usize;
                let i1 = (i0 + 1).min(bins - 1);
                let t = src_pos - i0 as f32;
                spec[i0] * (1.0 - t) + spec[i1] * t
            } else {
                realfft::num_complex::Complex::new(0.0, 0.0)
            };
            if shift != 0.0 {
                cum_phase[bin] += shift * phase_per_shift;
                any_phase = true;
            }
            let ph = cum_phase[bin];
            if ph != 0.0 {
                let (sin, cos) = ph.sin_cos();
                v *= realfft::num_complex::Complex::new(cos, sin);
            }
            warped[bin] = v;
        }
        warped[0].im = 0.0;
        if let Some(last) = warped.last_mut() {
            last.im = 0.0;
        }
        spec.copy_from_slice(&warped);
        if irfft.process(&mut spec, &mut time_out).is_err() {
            return signal.to_vec();
        }
        for i in 0..win {
            out[start + i] += time_out[i] * inv_win * window[i];
            norm[start + i] += window[i] * window[i];
        }
    }
    for i in 0..out_len {
        out[i] /= norm[i].max(1e-8);
    }
    out[pad..pad + n].to_vec()
}

/// Apply spectral-warp `points` (absolute sample positions) to one channel,
/// processing only the influenced region (plus STFT margins) and crossfading
/// the region edges against the original so nothing clicks. Radii are the
/// Gaussian sigmas: `time_radius_ms` in milliseconds, `freq_radius_hz` in Hz.
pub(crate) fn warp_channel_with_points(
    ch: &[f32],
    sr: u32,
    points: &[crate::app::types::SpectralWarpPoint],
    time_radius_ms: f32,
    freq_radius_hz: f32,
) -> Vec<f32> {
    let n = ch.len();
    if n == 0 || points.is_empty() {
        return ch.to_vec();
    }
    let time_sigma = (time_radius_ms.max(1.0) / 1000.0) * sr.max(1) as f32;
    let reach = (time_sigma * 3.0).ceil() as usize + SPECTRAL_FFT_SIZE * 2;
    let min_t = points.iter().map(|p| p.sample).min().unwrap_or(0);
    let max_t = points.iter().map(|p| p.sample).max().unwrap_or(0);
    let seg_s = min_t.saturating_sub(reach);
    let seg_e = (max_t + reach).min(n);
    if seg_e <= seg_s {
        return ch.to_vec();
    }
    let rel_points: Vec<(f32, f32, f32)> = points
        .iter()
        .map(|p| (p.sample as f32 - seg_s as f32, p.freq_hz, p.delta_hz))
        .collect();
    let processed = stft_warp_frequency(
        &ch[seg_s..seg_e],
        sr,
        &rel_points,
        time_sigma,
        freq_radius_hz.max(1.0),
    );
    let mut out = ch.to_vec();
    let seg_len = seg_e - seg_s;
    // Edge fade only where the segment abuts untouched audio.
    let fade_n = SPECTRAL_FFT_SIZE.min(seg_len / 4);
    for i in 0..seg_len {
        let mut w = 1.0f32;
        if seg_s > 0 && i < fade_n {
            w = w.min(raised_cosine((i as f32 + 0.5) / fade_n as f32));
        }
        if seg_e < n && i + fade_n >= seg_len {
            let from_end = seg_len - 1 - i;
            w = w.min(raised_cosine((from_end as f32 + 0.5) / fade_n as f32));
        }
        out[seg_s + i] = ch[seg_s + i] * (1.0 - w) + processed[i] * w;
    }
    out
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

    /// True when the tab has at least one spectral-warp stroke that would
    /// actually move content.
    pub(super) fn editor_spectral_warp_ready(tab: &crate::app::types::EditorTab) -> bool {
        tab.spectral_warp_points
            .iter()
            .any(|p| p.delta_hz.abs() > 1.0)
    }

    /// Render the current spectral-warp points into a non-destructive
    /// preview on a worker thread: mono audition through the heavy-preview
    /// channel and a green per-channel overlay through the heavy-overlay
    /// channel (both drained by the existing pollers).
    pub(super) fn spawn_spectral_warp_preview_for_tab(&mut self, tab_idx: usize) {
        use std::sync::mpsc;
        let Some(tab) = self.tabs.get(tab_idx) else {
            return;
        };
        if tab.loading || !Self::editor_spectral_warp_ready(tab) {
            return;
        }
        let path = tab.path.clone();
        let channels = tab.ch_samples.clone();
        let samples_len = tab.samples_len;
        let sr = tab.buffer_sample_rate.max(1);
        let points = tab.spectral_warp_points.clone();
        let time_radius_ms = tab.tool_state.warp_time_radius_ms.max(1.0);
        let freq_radius_hz = tab.tool_state.warp_freq_radius_hz.max(1.0);

        self.audio.stop();
        if let Some(tab) = self.tabs.get_mut(tab_idx) {
            tab.preview_audio_tool = Some(crate::app::types::ToolKind::SpectralWarp);
        }
        self.clear_heavy_preview_state();
        self.clear_heavy_overlay_state();
        self.heavy_preview_gen_counter = self.heavy_preview_gen_counter.wrapping_add(1);
        let preview_gen = self.heavy_preview_gen_counter;
        self.heavy_preview_expected_gen = preview_gen;
        self.heavy_preview_expected_path = Some(path.clone());
        self.heavy_preview_expected_tool = Some(crate::app::types::ToolKind::SpectralWarp);
        self.overlay_gen_counter = self.overlay_gen_counter.wrapping_add(1);
        let overlay_gen = self.overlay_gen_counter;
        self.overlay_expected_gen = overlay_gen;
        self.overlay_expected_path = Some(path.clone());
        self.overlay_expected_tool = Some(crate::app::types::ToolKind::SpectralWarp);

        let (preview_tx, preview_rx) = mpsc::channel::<super::HeavyPreviewMessage>();
        let (overlay_tx, overlay_rx) = mpsc::channel::<super::HeavyOverlayMessage>();
        std::thread::spawn(move || {
            let processed: Vec<Vec<f32>> = channels
                .iter()
                .map(|ch| warp_channel_with_points(ch, sr, &points, time_radius_ms, freq_radius_hz))
                .collect();
            let mono = crate::app::WavesPreviewer::mixdown_channels(&processed, samples_len);
            let timeline_len = processed.get(0).map(Vec::len).unwrap_or(samples_len).max(1);
            let overlay = crate::app::WavesPreviewer::preview_overlay_from_channels(
                processed,
                crate::app::types::ToolKind::SpectralWarp,
                timeline_len,
            );
            let _ = overlay_tx.send((
                path.clone(),
                crate::app::types::ToolKind::SpectralWarp,
                overlay,
                overlay_gen,
                true,
            ));
            if !mono.is_empty() {
                let _ = preview_tx.send((
                    path,
                    crate::app::types::ToolKind::SpectralWarp,
                    mono,
                    preview_gen,
                ));
            }
        });
        self.heavy_preview_rx = Some(preview_rx);
        self.heavy_overlay_rx = Some(overlay_rx);
    }

    /// Destructively apply the current spectral-warp points on a worker
    /// thread through the shared apply pipeline (busy overlay + undo).
    /// The points are consumed: they describe an edit relative to the
    /// pre-warp audio, so they are cleared once the job is queued.
    pub(super) fn spawn_spectral_warp_apply_for_tab(&mut self, tab_idx: usize) {
        use std::sync::mpsc;
        let Some(tab) = self.tabs.get(tab_idx) else {
            return;
        };
        if tab.loading || !Self::editor_spectral_warp_ready(tab) {
            return;
        }
        let undo = Some(Self::capture_undo_state(tab));
        let channels = tab.ch_samples.clone();
        let sr = tab.buffer_sample_rate.max(1);
        let points = tab.spectral_warp_points.clone();
        let time_radius_ms = tab.tool_state.warp_time_radius_ms.max(1.0);
        let freq_radius_hz = tab.tool_state.warp_freq_radius_hz.max(1.0);
        self.editor_apply_state = None;
        self.audio.stop();
        if let Some(tab) = self.tabs.get_mut(tab_idx) {
            tab.spectral_warp_points.clear();
            tab.spectral_warp_drag = None;
            tab.preview_audio_tool = None;
            tab.preview_overlay = None;
        }
        let (tx, rx) = mpsc::channel::<crate::app::types::EditorApplyResult>();
        std::thread::spawn(move || {
            let out: Vec<Vec<f32>> = channels
                .iter()
                .map(|ch| warp_channel_with_points(ch, sr, &points, time_radius_ms, freq_radius_hz))
                .collect();
            let len = out.get(0).map(Vec::len).unwrap_or(0);
            let mono = crate::app::WavesPreviewer::mixdown_channels(&out, len);
            let (waveform_minmax, waveform_pyramid) =
                crate::app::WavesPreviewer::build_editor_waveform_cache(&out, len);
            let channels_arc = std::sync::Arc::new(out.clone());
            let _ = tx.send(crate::app::types::EditorApplyResult {
                tab_idx,
                samples: mono,
                channels: out,
                channels_arc,
                waveform_minmax,
                waveform_pyramid,
                lufs_override: None,
                selection_after: None,
            });
        });
        self.editor_apply_state = Some(crate::app::types::EditorApplyState {
            msg: "Applying Spectral Warp...".to_string(),
            rx,
            tab_idx,
            undo,
        });
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

    /// Magnitude of `sig` at `freq` via Goertzel.
    fn goertzel(sig: &[f32], sr: u32, freq: f32) -> f32 {
        let w = 2.0 * core::f32::consts::PI * freq / sr as f32;
        let coeff = 2.0 * w.cos();
        let (mut s1, mut s2) = (0.0f32, 0.0f32);
        for &x in sig {
            let s0 = x + coeff * s1 - s2;
            s2 = s1;
            s1 = s0;
        }
        ((s1 * s1 + s2 * s2 - coeff * s1 * s2) / (sig.len() as f32 * sig.len() as f32 / 4.0))
            .max(0.0)
            .sqrt()
    }

    #[test]
    fn warp_no_points_is_identity() {
        let sr = 48_000u32;
        let sig = sine(1_000.0, sr, 24_000);
        let out = warp_channel_with_points(&sig, sr, &[], 150.0, 300.0);
        assert_eq!(out, sig);
    }

    #[test]
    fn warp_zero_delta_is_transparent() {
        let sr = 48_000u32;
        let len = 24_000usize;
        let sig = sine(1_000.0, sr, len);
        let pts = [crate::app::types::SpectralWarpPoint {
            sample: len / 2,
            freq_hz: 1_000.0,
            delta_hz: 0.0,
        }];
        let out = warp_channel_with_points(&sig, sr, &pts, 150.0, 300.0);
        let mid = len / 4..(3 * len / 4);
        let max_err = out[mid.clone()]
            .iter()
            .zip(&sig[mid])
            .map(|(o, s)| (o - s).abs())
            .fold(0.0f32, f32::max);
        assert!(max_err < 1e-3, "zero-delta warp not transparent: {max_err}");
    }

    #[test]
    fn warp_shifts_sine_up() {
        let sr = 48_000u32;
        let len = 48_000usize; // 1 s
        let sig = sine(1_000.0, sr, len);
        // Drag the 1 kHz content up by 500 Hz in the middle of the file,
        // with radii wide enough to cover the analysis window.
        let pts = [crate::app::types::SpectralWarpPoint {
            sample: len / 2,
            freq_hz: 1_000.0,
            delta_hz: 500.0,
        }];
        let out = warp_channel_with_points(&sig, sr, &pts, 120.0, 400.0);
        assert_eq!(out.len(), sig.len());
        // Measure around the warp center (within ~1 sigma of the point).
        let probe = &out[len / 2 - 2_000..len / 2 + 2_000];
        let at_target = goertzel(probe, sr, 1_500.0);
        let at_origin = goertzel(probe, sr, 1_000.0);
        let source = goertzel(&sig[len / 2 - 2_000..len / 2 + 2_000], sr, 1_000.0);
        // The shift tapers with the Gaussian time falloff across the probe
        // window, so the moved partial is slightly smeared around 1.5 kHz —
        // expect a solid fraction of the source amplitude, not all of it.
        assert!(
            at_target > source * 0.25,
            "warped energy missing at 1.5 kHz: target={at_target} source={source}"
        );
        assert!(
            at_target > at_origin,
            "energy did not move: target={at_target} origin={at_origin}"
        );
        assert!(
            at_origin < source * 0.5,
            "origin energy not attenuated: origin={at_origin} source={source}"
        );
        // Far away from the warp point (in time) the sine is untouched.
        let far = &out[2_000..6_000];
        let far_src = &sig[2_000..6_000];
        let far_orig = goertzel(far, sr, 1_000.0);
        let far_ref = goertzel(far_src, sr, 1_000.0);
        assert!(
            (far_orig - far_ref).abs() < far_ref * 0.1,
            "audio far from the warp changed: {far_orig} vs {far_ref}"
        );
    }

    #[test]
    fn warp_region_is_bounded() {
        let sr = 48_000u32;
        let len = 96_000usize; // 2 s
        let sig = sine(2_000.0, sr, len);
        let pts = [crate::app::types::SpectralWarpPoint {
            sample: len / 2,
            freq_hz: 2_000.0,
            delta_hz: 300.0,
        }];
        let out = warp_channel_with_points(&sig, sr, &pts, 50.0, 300.0);
        // Outside the influenced region the samples are bit-identical.
        let reach = ((50.0 / 1000.0) * sr as f32 * 3.0) as usize + SPECTRAL_FFT_SIZE * 2;
        let seg_s = len / 2 - reach;
        let seg_e = len / 2 + reach;
        assert_eq!(&out[..seg_s], &sig[..seg_s]);
        assert_eq!(&out[seg_e..], &sig[seg_e..]);
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
