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

/// Generic STFT frame engine: Hann analysis/synthesis windows, 75%
/// overlap, weighted overlap-add, reflect padding of half a window on
/// both sides so the edges reconstruct exactly like the interior.
/// `process` runs once per frame with the frame's center position in
/// signal coordinates and the mutable half-spectrum. Returns a signal of
/// the same length (the input unchanged on FFT failure).
fn stft_process_frames<F>(signal: &[f32], mut process: F) -> Vec<f32>
where
    F: FnMut(f32, &mut [realfft::num_complex::Complex<f32>]),
{
    let n = signal.len();
    if n == 0 {
        return Vec::new();
    }
    let win = SPECTRAL_FFT_SIZE;
    let hop = SPECTRAL_HOP_SIZE;

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
        // With pad == win/2 the center of frame k sits at k*hop in
        // signal coordinates.
        let t_center = (frame_idx * hop) as f32;
        for i in 0..win {
            frame[i] = padded[start + i] * window[i];
        }
        if rfft.process(&mut frame, &mut spec).is_err() {
            return signal.to_vec();
        }
        process(t_center, &mut spec);
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

/// Apply a per-bin gain mask to `signal` via STFT → mask → weighted
/// overlap-add ISTFT. Returns a signal of the same length.
fn stft_apply_band_gain(
    signal: &[f32],
    sr: u32,
    lo_hz: f32,
    hi_hz: f32,
    fade_hz: f32,
    keep_band: bool,
) -> Vec<f32> {
    let gains = band_bin_gains(SPECTRAL_FFT_SIZE, sr, lo_hz, hi_hz, fade_hz, keep_band);
    stft_process_frames(signal, |_t, spec| {
        for (bin, v) in spec.iter_mut().enumerate() {
            *v *= gains[bin.min(gains.len() - 1)];
        }
    })
}

/// Hard ceiling for accumulated brush attenuation, so stacked strokes
/// converge to "silence" instead of denormal territory.
const MAX_BRUSH_CUT_DB: f32 = 80.0;

/// Paint-out attenuation for one channel: each stamp cuts spectrogram
/// magnitude around (`sample`, `freq_hz`) by up to its `cut_db`, with
/// Gaussian falloff in time and frequency (sigmas baked into the stamp).
/// Overlapping stamps add in dB (clamped to [`MAX_BRUSH_CUT_DB`]). Only
/// the influenced region (plus STFT margins) is processed; its edges are
/// crossfaded against the original so nothing clicks, and audio outside
/// the region is returned bit-identical.
pub(crate) fn brush_channel_with_stamps(
    ch: &[f32],
    sr: u32,
    stamps: &[crate::app::types::SpectralBrushStamp],
) -> Vec<f32> {
    let n = ch.len();
    if n == 0 || stamps.is_empty() {
        return ch.to_vec();
    }
    let sr_f = sr.max(1) as f32;
    let max_sigma_samples = stamps
        .iter()
        .map(|s| (s.time_sigma_ms.max(1.0) / 1000.0) * sr_f)
        .fold(1.0f32, f32::max);
    let reach = (max_sigma_samples * 3.0).ceil() as usize + SPECTRAL_FFT_SIZE * 2;
    let min_t = stamps.iter().map(|p| p.sample).min().unwrap_or(0);
    let max_t = stamps.iter().map(|p| p.sample).max().unwrap_or(0);
    let seg_s = min_t.saturating_sub(reach);
    let seg_e = (max_t + reach).min(n);
    if seg_e <= seg_s {
        return ch.to_vec();
    }

    let mut sorted: Vec<crate::app::types::SpectralBrushStamp> = stamps.to_vec();
    sorted.sort_by_key(|s| s.sample);
    let bins = SPECTRAL_FFT_SIZE / 2 + 1;
    let hz_per_bin = sr_f / SPECTRAL_FFT_SIZE as f32;
    // Sliding window into `sorted`: stamps are sample-ordered and frames
    // advance monotonically, so each stamp is skipped past exactly once
    // (O(frames + stamps) window management, not O(frames * stamps)).
    let mut lo_idx = 0usize;
    let mut cut_db = vec![0.0f32; bins];

    let processed = stft_process_frames(&ch[seg_s..seg_e], |t_center, spec| {
        let t_abs = t_center + seg_s as f32;
        while lo_idx < sorted.len() {
            let s = &sorted[lo_idx];
            let sigma_t = (s.time_sigma_ms.max(1.0) / 1000.0) * sr_f;
            if (s.sample as f32) + sigma_t * 3.0 < t_abs {
                lo_idx += 1;
            } else {
                break;
            }
        }
        cut_db.iter_mut().for_each(|v| *v = 0.0);
        let mut any = false;
        for s in &sorted[lo_idx..] {
            let sigma_t = (s.time_sigma_ms.max(1.0) / 1000.0) * sr_f;
            let dt = s.sample as f32 - t_abs;
            if dt > max_sigma_samples * 3.0 {
                // Every later stamp starts even further ahead of this
                // frame than the widest possible reach.
                break;
            }
            if dt.abs() > sigma_t * 3.0 {
                continue;
            }
            let wt = (-0.5 * (dt / sigma_t) * (dt / sigma_t)).exp();
            let sigma_f = s.freq_sigma_hz.max(hz_per_bin);
            let b_lo = (((s.freq_hz - sigma_f * 3.0) / hz_per_bin).floor().max(0.0)) as usize;
            let b_hi = (((s.freq_hz + sigma_f * 3.0) / hz_per_bin).ceil() as usize).min(bins - 1);
            for bin in b_lo..=b_hi.min(bins - 1) {
                let df = (bin as f32 * hz_per_bin - s.freq_hz) / sigma_f;
                cut_db[bin] += s.cut_db.max(0.0) * wt * (-0.5 * df * df).exp();
                any = true;
            }
        }
        if !any {
            return;
        }
        for (bin, v) in spec.iter_mut().enumerate() {
            let db = cut_db[bin.min(bins - 1)].min(MAX_BRUSH_CUT_DB);
            if db > 1e-3 {
                *v *= 10f32.powf(-db / 20.0);
            }
        }
    });

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

/// Learn a per-bin average noise magnitude from `[s, e)` of one channel.
/// Returns one magnitude per STFT bin (the profile for this channel).
pub(crate) fn learn_noise_profile_channel(ch: &[f32], s: usize, e: usize) -> Vec<f32> {
    let bins = SPECTRAL_FFT_SIZE / 2 + 1;
    let n = ch.len();
    let e = e.min(n);
    if s >= e {
        return vec![0.0f32; bins];
    }
    let mut acc = vec![0.0f64; bins];
    let mut count = 0usize;
    let _ = stft_process_frames(&ch[s..e], |_t, spec| {
        for (bin, a) in acc.iter_mut().enumerate() {
            *a += spec[bin.min(spec.len() - 1)].norm() as f64;
        }
        count += 1;
    });
    if count == 0 {
        return vec![0.0f32; bins];
    }
    acc.iter().map(|a| (*a / count as f64) as f32).collect()
}

/// Profile-based spectral subtraction de-noise for one channel.
/// Per bin: `g = clamp(1 - strength * (N/|X|)^2, floor, 1)` with
/// `floor = 10^(-reduction_db/20)` (the maximum attenuation), then an
/// asymmetric one-pole across frames (slow release, fast attack) to tame
/// musical noise. `range = None` processes the whole channel; `Some`
/// processes only that region (plus STFT margins) with crossfaded edges,
/// leaving the rest bit-identical.
pub(crate) fn denoise_channel(
    ch: &[f32],
    profile: &[f32],
    reduction_db: f32,
    strength: f32,
    range: Option<(usize, usize)>,
) -> Vec<f32> {
    let n = ch.len();
    let bins = SPECTRAL_FFT_SIZE / 2 + 1;
    if n == 0 || profile.len() != bins || profile.iter().all(|&m| m <= 0.0) {
        return ch.to_vec();
    }
    let floor = 10f32.powf(-reduction_db.clamp(0.0, 80.0) / 20.0);
    let strength = strength.clamp(1.0, 4.0);
    let (seg_s, seg_e) = match range {
        Some((s, e)) => {
            let e = e.min(n);
            if s >= e {
                return ch.to_vec();
            }
            (
                s.saturating_sub(SPECTRAL_FFT_SIZE * 2),
                (e + SPECTRAL_FFT_SIZE * 2).min(n),
            )
        }
        None => (0, n),
    };

    let mut g_prev = vec![1.0f32; bins];
    let processed = stft_process_frames(&ch[seg_s..seg_e], |_t, spec| {
        for bin in 0..bins.min(spec.len()) {
            let x = spec[bin].norm();
            let ratio = if x > 1e-12 { profile[bin] / x } else { 1.0 };
            let g_raw = (1.0 - strength * ratio * ratio).clamp(floor, 1.0);
            // Asymmetric smoothing: gains fall (more suppression) quickly,
            // recover slowly — flickering bins are what reads as
            // "musical noise".
            let a = if g_raw > g_prev[bin] { 0.6 } else { 0.25 };
            let g = g_prev[bin] * a + g_raw * (1.0 - a);
            g_prev[bin] = g;
            spec[bin] *= g.clamp(floor, 1.0);
        }
    });

    let mut out = ch.to_vec();
    match range {
        None => out.copy_from_slice(&processed),
        Some((s, e)) => {
            let e = e.min(n);
            let sel_len = e - s;
            // Same edge treatment as the other region-scoped spectral
            // edits: raised-cosine into the untouched audio.
            let fade_n = (SPECTRAL_FFT_SIZE / 2).min(sel_len / 2).max(1);
            for i in 0..sel_len {
                let w = selection_edge_weight(i, sel_len, fade_n);
                out[s + i] = ch[s + i] * (1.0 - w) + processed[(s - seg_s) + i] * w;
            }
        }
    }
    out
}

/// Frames of spectral context averaged on each side of a healed region.
const HEAL_CONTEXT_FRAMES: usize = 4;
/// Selections longer than this are refused by the Heal button (the whole
/// region is STFT-buffered in memory twice).
pub(crate) const HEAL_MAX_SELECTION_SECS: f32 = 120.0;

/// RX-style inpaint of `[s, e)` from its spectral context: per-bin
/// magnitudes interpolate linearly in time between the averages of
/// [`HEAL_CONTEXT_FRAMES`] frames on each side, and phase advances from
/// the left context's measured per-bin phase velocity so steady tones
/// bridge the gap coherently. With `band = Some((lo, hi))` only that band
/// is replaced (raised-cosine edges of `freq_fade_hz`); the time edges
/// crossfade over `time_fade_ms`. Samples outside `[s, e)` are returned
/// bit-identical. Degenerate or context-free selections return the input.
pub(crate) fn heal_channel_range(
    ch: &[f32],
    sr: u32,
    s: usize,
    e: usize,
    band: Option<(f32, f32)>,
    freq_fade_hz: f32,
    time_fade_ms: f32,
) -> Vec<f32> {
    use realfft::num_complex::Complex;
    let n = ch.len();
    let e = e.min(n);
    if s >= e || n == 0 {
        return ch.to_vec();
    }
    let win = SPECTRAL_FFT_SIZE;
    let hop = SPECTRAL_HOP_SIZE;
    let bins = win / 2 + 1;
    let ctx_span = (HEAL_CONTEXT_FRAMES + 2) * win;
    let seg_s = s.saturating_sub(ctx_span);
    let seg_e = (e + ctx_span).min(n);
    let seg = &ch[seg_s..seg_e];

    // Pass 1: collect every analysis frame's spectrum.
    let mut frames: Vec<Vec<Complex<f32>>> = Vec::new();
    let _ = stft_process_frames(seg, |_t, spec| frames.push(spec.to_vec()));
    if frames.is_empty() {
        return ch.to_vec();
    }

    // Frame k's center sits at k*hop in segment coordinates. Interior
    // frames are the ones whose centers fall inside the selection.
    let sel_s = (s - seg_s) as f32;
    let sel_e = (e - seg_s) as f32;
    let first_in = ((sel_s / hop as f32).ceil() as usize).min(frames.len());
    let last_ex = ((sel_e / hop as f32).ceil() as usize).min(frames.len());
    if first_in >= last_ex {
        return ch.to_vec();
    }
    let left_s = first_in.saturating_sub(HEAL_CONTEXT_FRAMES);
    let right_e = (last_ex + HEAL_CONTEXT_FRAMES).min(frames.len());
    let has_left = left_s < first_in;
    let has_right = last_ex < right_e;
    if !has_left && !has_right {
        return ch.to_vec();
    }

    let avg_mag = |range: core::ops::Range<usize>| -> Vec<f32> {
        let count = range.len().max(1) as f32;
        let mut mag = vec![0.0f32; bins];
        for k in range {
            for (bin, m) in mag.iter_mut().enumerate() {
                *m += frames[k][bin].norm();
            }
        }
        mag.iter_mut().for_each(|m| *m /= count);
        mag
    };
    let left_mag = if has_left {
        avg_mag(left_s..first_in)
    } else {
        avg_mag(last_ex..right_e)
    };
    let right_mag = if has_right {
        avg_mag(last_ex..right_e)
    } else {
        left_mag.clone()
    };

    // Phase seed and per-frame advance from the last two left-context
    // frames (falling back to each bin's natural advance of
    // 2*pi*bin*hop/win when there is no usable left context).
    let phase_ref = if has_left { first_in - 1 } else { last_ex };
    let mut phase0 = vec![0.0f32; bins];
    let mut dphi = vec![0.0f32; bins];
    for bin in 0..bins {
        phase0[bin] = frames[phase_ref][bin].arg();
        let natural = 2.0 * core::f32::consts::PI * bin as f32 * hop as f32 / win as f32;
        dphi[bin] = if has_left && phase_ref > 0 {
            let prev = frames[phase_ref - 1][bin];
            let cur = frames[phase_ref][bin];
            if prev.norm() > 1e-9 && cur.norm() > 1e-9 {
                (cur * prev.conj()).arg()
            } else {
                natural
            }
        } else {
            natural
        };
    }

    let wband: Vec<f32> = match band {
        Some((lo, hi)) => band_bin_gains(win, sr, lo, hi, freq_fade_hz.max(1.0), true),
        None => vec![1.0f32; bins],
    };

    // Pass 2: resynthesize with the interior frames rebuilt.
    let denom = (last_ex - first_in).max(1) as f32;
    let mut k = 0usize;
    let healed = stft_process_frames(seg, |_t, spec| {
        let idx = k;
        k += 1;
        if idx < first_in || idx >= last_ex {
            return;
        }
        let u = (idx - first_in) as f32 / denom;
        let steps = (idx - phase_ref) as f32;
        for bin in 0..bins.min(spec.len()) {
            let w = wband[bin];
            if w <= 0.0 {
                continue;
            }
            let mag = left_mag[bin] * (1.0 - u) + right_mag[bin] * u;
            let ph = phase0[bin] + dphi[bin] * steps;
            let (sin, cos) = ph.sin_cos();
            let repaired = Complex::new(mag * cos, mag * sin);
            spec[bin] = spec[bin] * (1.0 - w) + repaired * w;
        }
    });

    // Write back only inside the selection, with click-free time edges.
    let mut out = ch.to_vec();
    let sel_len = e - s;
    let fade_n = ((time_fade_ms.max(3.0) / 1000.0) * sr.max(1) as f32).round() as usize;
    let fade_n = fade_n.min(sel_len / 2);
    for i in 0..sel_len {
        let w = selection_edge_weight(i, sel_len, fade_n);
        out[s + i] = ch[s + i] * (1.0 - w) + healed[(s - seg_s) + i] * w;
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
        if self.editor_apply_state.is_some() {
            return;
        }
        let apply_tab_id = tab.tab_id;
        if matches!(&self.playback_session.source,
            crate::app::PlaybackSourceKind::EditorTab(p) if *p == tab.path)
        {
            self.audio.stop();
        }
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
            tab_id: apply_tab_id,
            undo,
        });
    }

    /// True when the tab has at least one spectral-brush stamp that would
    /// actually attenuate content.
    pub(super) fn editor_spectral_brush_ready(tab: &crate::app::types::EditorTab) -> bool {
        tab.spectral_brush_stamps.iter().any(|s| s.cut_db > 0.1)
    }

    /// Render the current spectral-brush stamps into a non-destructive
    /// preview on a worker thread (same channels as the warp preview).
    pub(super) fn spawn_spectral_brush_preview_for_tab(&mut self, tab_idx: usize) {
        use std::sync::mpsc;
        let Some(tab) = self.tabs.get(tab_idx) else {
            return;
        };
        if tab.loading || !Self::editor_spectral_brush_ready(tab) {
            return;
        }
        let path = tab.path.clone();
        let channels = tab.ch_samples.clone();
        let samples_len = tab.samples_len;
        let sr = tab.buffer_sample_rate.max(1);
        let stamps = tab.spectral_brush_stamps.clone();

        self.audio.stop();
        if let Some(tab) = self.tabs.get_mut(tab_idx) {
            tab.preview_audio_tool = Some(crate::app::types::ToolKind::SpectralBrush);
        }
        self.clear_heavy_preview_state();
        self.clear_heavy_overlay_state();
        self.heavy_preview_gen_counter = self.heavy_preview_gen_counter.wrapping_add(1);
        let preview_gen = self.heavy_preview_gen_counter;
        self.heavy_preview_expected_gen = preview_gen;
        self.heavy_preview_expected_path = Some(path.clone());
        self.heavy_preview_expected_tool = Some(crate::app::types::ToolKind::SpectralBrush);
        self.overlay_gen_counter = self.overlay_gen_counter.wrapping_add(1);
        let overlay_gen = self.overlay_gen_counter;
        self.overlay_expected_gen = overlay_gen;
        self.overlay_expected_path = Some(path.clone());
        self.overlay_expected_tool = Some(crate::app::types::ToolKind::SpectralBrush);

        let (preview_tx, preview_rx) = mpsc::channel::<super::HeavyPreviewMessage>();
        let (overlay_tx, overlay_rx) = mpsc::channel::<super::HeavyOverlayMessage>();
        std::thread::spawn(move || {
            let processed: Vec<Vec<f32>> = channels
                .iter()
                .map(|ch| brush_channel_with_stamps(ch, sr, &stamps))
                .collect();
            let mono = crate::app::WavesPreviewer::mixdown_channels(&processed, samples_len);
            let timeline_len = processed.get(0).map(Vec::len).unwrap_or(samples_len).max(1);
            let overlay = crate::app::WavesPreviewer::preview_overlay_from_channels(
                processed,
                crate::app::types::ToolKind::SpectralBrush,
                timeline_len,
            );
            let _ = overlay_tx.send((
                path.clone(),
                crate::app::types::ToolKind::SpectralBrush,
                overlay,
                overlay_gen,
                true,
            ));
            if !mono.is_empty() {
                let _ = preview_tx.send((
                    path,
                    crate::app::types::ToolKind::SpectralBrush,
                    mono,
                    preview_gen,
                ));
            }
        });
        self.heavy_preview_rx = Some(preview_rx);
        self.heavy_overlay_rx = Some(overlay_rx);
    }

    /// Destructively apply the current spectral-brush stamps on a worker
    /// thread through the shared apply pipeline (busy overlay + undo).
    /// The stamps are consumed once the job is queued.
    pub(super) fn spawn_spectral_brush_apply_for_tab(&mut self, tab_idx: usize) {
        use std::sync::mpsc;
        let Some(tab) = self.tabs.get(tab_idx) else {
            return;
        };
        if tab.loading || !Self::editor_spectral_brush_ready(tab) {
            return;
        }
        let undo = Some(Self::capture_undo_state(tab));
        let channels = tab.ch_samples.clone();
        let sr = tab.buffer_sample_rate.max(1);
        let stamps = tab.spectral_brush_stamps.clone();
        if self.editor_apply_state.is_some() {
            return;
        }
        let apply_tab_id = tab.tab_id;
        if matches!(&self.playback_session.source,
            crate::app::PlaybackSourceKind::EditorTab(p) if *p == tab.path)
        {
            self.audio.stop();
        }
        if let Some(tab) = self.tabs.get_mut(tab_idx) {
            tab.spectral_brush_stamps.clear();
            tab.spectral_brush_last = None;
            tab.preview_audio_tool = None;
            tab.preview_overlay = None;
        }
        let (tx, rx) = mpsc::channel::<crate::app::types::EditorApplyResult>();
        std::thread::spawn(move || {
            let out: Vec<Vec<f32>> = channels
                .iter()
                .map(|ch| brush_channel_with_stamps(ch, sr, &stamps))
                .collect();
            let len = out.get(0).map(Vec::len).unwrap_or(0);
            let mono = crate::app::WavesPreviewer::mixdown_channels(&out, len);
            let (waveform_minmax, waveform_pyramid) =
                crate::app::WavesPreviewer::build_editor_waveform_cache(&out, len);
            let channels_arc = std::sync::Arc::new(out.clone());
            let _ = tx.send(crate::app::types::EditorApplyResult {
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
            msg: "Applying Spectral Brush...".to_string(),
            rx,
            tab_id: apply_tab_id,
            undo,
        });
    }

    /// Learn a noise profile from the current selection (synchronous; the
    /// selection is capped at 60 s which the STFT sweeps in well under a
    /// frame's budget for typical files).
    pub(super) fn editor_denoise_learn_profile(&mut self, tab_idx: usize) {
        const LEARN_MAX_SECS: f32 = 60.0;
        let Some(tab) = self.tabs.get(tab_idx) else {
            return;
        };
        if tab.loading {
            return;
        }
        let Some((s, mut e)) = Self::editor_valid_selection(tab) else {
            self.push_toast(
                crate::app::types::ToastSeverity::Warning,
                "De-noise: select a noise-only region first, then Learn",
            );
            return;
        };
        let sr = tab.buffer_sample_rate.max(1);
        let max_len = (LEARN_MAX_SECS * sr as f32) as usize;
        if e - s > max_len {
            e = s + max_len;
        }
        let mag_per_channel: Vec<Vec<f32>> = tab
            .ch_samples
            .iter()
            .map(|ch| learn_noise_profile_channel(ch, s, e))
            .collect();
        let profile = crate::app::types::NoiseProfile {
            fft_size: SPECTRAL_FFT_SIZE,
            sample_rate: sr,
            mag_per_channel,
            learned_from_ms: (
                s as f32 * 1000.0 / sr as f32,
                e as f32 * 1000.0 / sr as f32,
            ),
        };
        if let Some(tab) = self.tabs.get_mut(tab_idx) {
            tab.noise_profile = Some(profile);
        }
        self.push_toast(
            crate::app::types::ToastSeverity::Info,
            "De-noise: profile learned from selection",
        );
    }

    /// True when the tab has a noise profile usable against its current
    /// buffer (same FFT layout and sample rate).
    pub(super) fn editor_denoise_ready(tab: &crate::app::types::EditorTab) -> bool {
        tab.noise_profile
            .as_ref()
            .map(|p| {
                p.fft_size == SPECTRAL_FFT_SIZE
                    && p.sample_rate == tab.buffer_sample_rate.max(1)
                    && !p.mag_per_channel.is_empty()
            })
            .unwrap_or(false)
    }

    fn denoise_processed_channels(
        channels: &[Vec<f32>],
        profile: &crate::app::types::NoiseProfile,
        reduction_db: f32,
        strength: f32,
        range: Option<(usize, usize)>,
    ) -> Vec<Vec<f32>> {
        channels
            .iter()
            .enumerate()
            .map(|(ci, ch)| {
                let mags = profile
                    .mag_per_channel
                    .get(ci)
                    .or_else(|| profile.mag_per_channel.first());
                match mags {
                    Some(mags) => denoise_channel(ch, mags, reduction_db, strength, range),
                    None => ch.clone(),
                }
            })
            .collect()
    }

    /// Render the de-noise into a non-destructive preview on a worker
    /// thread (same channels as the warp/brush previews).
    pub(super) fn spawn_denoise_preview_for_tab(&mut self, tab_idx: usize) {
        use std::sync::mpsc;
        let Some(tab) = self.tabs.get(tab_idx) else {
            return;
        };
        if tab.loading || !Self::editor_denoise_ready(tab) {
            return;
        }
        let path = tab.path.clone();
        let channels = tab.ch_samples.clone();
        let samples_len = tab.samples_len;
        let profile = tab.noise_profile.clone().unwrap();
        let reduction_db = tab.tool_state.denoise_reduction_db.max(0.0);
        let strength = tab.tool_state.denoise_strength.clamp(1.0, 4.0);
        let range = Self::editor_valid_selection(tab);

        self.audio.stop();
        if let Some(tab) = self.tabs.get_mut(tab_idx) {
            tab.preview_audio_tool = Some(crate::app::types::ToolKind::DeNoise);
        }
        self.clear_heavy_preview_state();
        self.clear_heavy_overlay_state();
        self.heavy_preview_gen_counter = self.heavy_preview_gen_counter.wrapping_add(1);
        let preview_gen = self.heavy_preview_gen_counter;
        self.heavy_preview_expected_gen = preview_gen;
        self.heavy_preview_expected_path = Some(path.clone());
        self.heavy_preview_expected_tool = Some(crate::app::types::ToolKind::DeNoise);
        self.overlay_gen_counter = self.overlay_gen_counter.wrapping_add(1);
        let overlay_gen = self.overlay_gen_counter;
        self.overlay_expected_gen = overlay_gen;
        self.overlay_expected_path = Some(path.clone());
        self.overlay_expected_tool = Some(crate::app::types::ToolKind::DeNoise);

        let (preview_tx, preview_rx) = mpsc::channel::<super::HeavyPreviewMessage>();
        let (overlay_tx, overlay_rx) = mpsc::channel::<super::HeavyOverlayMessage>();
        std::thread::spawn(move || {
            let processed =
                Self::denoise_processed_channels(&channels, &profile, reduction_db, strength, range);
            let mono = crate::app::WavesPreviewer::mixdown_channels(&processed, samples_len);
            let timeline_len = processed.get(0).map(Vec::len).unwrap_or(samples_len).max(1);
            let overlay = crate::app::WavesPreviewer::preview_overlay_from_channels(
                processed,
                crate::app::types::ToolKind::DeNoise,
                timeline_len,
            );
            let _ = overlay_tx.send((
                path.clone(),
                crate::app::types::ToolKind::DeNoise,
                overlay,
                overlay_gen,
                true,
            ));
            if !mono.is_empty() {
                let _ = preview_tx.send((
                    path,
                    crate::app::types::ToolKind::DeNoise,
                    mono,
                    preview_gen,
                ));
            }
        });
        self.heavy_preview_rx = Some(preview_rx);
        self.heavy_overlay_rx = Some(overlay_rx);
    }

    /// Destructively apply the de-noise on a worker thread through the
    /// shared apply pipeline (busy overlay + undo). The learned profile is
    /// kept so the user can iterate.
    pub(super) fn spawn_denoise_apply_for_tab(&mut self, tab_idx: usize) {
        use std::sync::mpsc;
        let Some(tab) = self.tabs.get(tab_idx) else {
            return;
        };
        if tab.loading || !Self::editor_denoise_ready(tab) {
            return;
        }
        let undo = Some(Self::capture_undo_state(tab));
        let channels = tab.ch_samples.clone();
        let profile = tab.noise_profile.clone().unwrap();
        let reduction_db = tab.tool_state.denoise_reduction_db.max(0.0);
        let strength = tab.tool_state.denoise_strength.clamp(1.0, 4.0);
        let range = Self::editor_valid_selection(tab);
        if self.editor_apply_state.is_some() {
            return;
        }
        let apply_tab_id = tab.tab_id;
        if matches!(&self.playback_session.source,
            crate::app::PlaybackSourceKind::EditorTab(p) if *p == tab.path)
        {
            self.audio.stop();
        }
        if let Some(tab) = self.tabs.get_mut(tab_idx) {
            tab.preview_audio_tool = None;
            tab.preview_overlay = None;
        }
        let (tx, rx) = mpsc::channel::<crate::app::types::EditorApplyResult>();
        std::thread::spawn(move || {
            let out =
                Self::denoise_processed_channels(&channels, &profile, reduction_db, strength, range);
            let len = out.get(0).map(Vec::len).unwrap_or(0);
            let mono = crate::app::WavesPreviewer::mixdown_channels(&out, len);
            let (waveform_minmax, waveform_pyramid) =
                crate::app::WavesPreviewer::build_editor_waveform_cache(&out, len);
            let channels_arc = std::sync::Arc::new(out.clone());
            let _ = tx.send(crate::app::types::EditorApplyResult {
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
            msg: "Applying De-noise...".to_string(),
            rx,
            tab_id: apply_tab_id,
            undo,
        });
    }

    #[cfg(feature = "kittest")]
    pub fn test_denoise_learn(&mut self) -> bool {
        let Some(tab_idx) = self.active_tab else {
            return false;
        };
        self.editor_denoise_learn_profile(tab_idx);
        self.tabs
            .get(tab_idx)
            .map(|t| t.noise_profile.is_some())
            .unwrap_or(false)
    }

    #[cfg(feature = "kittest")]
    pub fn test_denoise_apply(&mut self) -> bool {
        let Some(tab_idx) = self.active_tab else {
            return false;
        };
        self.spawn_denoise_apply_for_tab(tab_idx);
        self.editor_apply_state.is_some()
    }

    /// Heal (inpaint) the current selection from its spectral context on a
    /// worker thread through the shared apply pipeline (busy overlay +
    /// undo). With a frequency band selected only that band is rebuilt.
    pub(super) fn spawn_spectral_heal_apply_for_tab(&mut self, tab_idx: usize) {
        use std::sync::mpsc;
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
        let sr = tab.buffer_sample_rate.max(1);
        if (e - s) as f32 / sr as f32 > HEAL_MAX_SELECTION_SECS {
            self.push_toast(
                crate::app::types::ToastSeverity::Warning,
                format!(
                    "Heal: selection is longer than {HEAL_MAX_SELECTION_SECS:.0} s — select a shorter range"
                ),
            );
            return;
        }
        let band = tab.freq_selection;
        let undo = Some(Self::capture_undo_state(tab));
        let channels = tab.ch_samples.clone();
        if self.editor_apply_state.is_some() {
            return;
        }
        let apply_tab_id = tab.tab_id;
        if matches!(&self.playback_session.source,
            crate::app::PlaybackSourceKind::EditorTab(p) if *p == tab.path)
        {
            self.audio.stop();
        }
        if let Some(tab) = self.tabs.get_mut(tab_idx) {
            tab.preview_audio_tool = None;
            tab.preview_overlay = None;
        }
        let (tx, rx) = mpsc::channel::<crate::app::types::EditorApplyResult>();
        std::thread::spawn(move || {
            let out: Vec<Vec<f32>> = channels
                .iter()
                .map(|ch| heal_channel_range(ch, sr, s, e, band, freq_fade_hz, time_fade_ms))
                .collect();
            let len = out.get(0).map(Vec::len).unwrap_or(0);
            let mono = crate::app::WavesPreviewer::mixdown_channels(&out, len);
            let (waveform_minmax, waveform_pyramid) =
                crate::app::WavesPreviewer::build_editor_waveform_cache(&out, len);
            let channels_arc = std::sync::Arc::new(out.clone());
            let _ = tx.send(crate::app::types::EditorApplyResult {
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
            msg: "Healing selection...".to_string(),
            rx,
            tab_id: apply_tab_id,
            undo,
        });
    }

    #[cfg(feature = "kittest")]
    pub fn test_spectral_heal_apply(&mut self) -> bool {
        let Some(tab_idx) = self.active_tab else {
            return false;
        };
        self.spawn_spectral_heal_apply_for_tab(tab_idx);
        self.editor_apply_state.is_some()
    }

    #[cfg(feature = "kittest")]
    pub fn test_spectral_brush_stamp(&mut self, frac: f32, freq_hz: f32, cut_db: f32) -> bool {
        let Some(tab_idx) = self.active_tab else {
            return false;
        };
        let Some(tab) = self.tabs.get_mut(tab_idx) else {
            return false;
        };
        if tab.samples_len == 0 {
            return false;
        }
        let sample = ((tab.samples_len as f32) * frac.clamp(0.0, 1.0)) as usize;
        tab.spectral_brush_stamps
            .push(crate::app::types::SpectralBrushStamp {
                sample,
                freq_hz,
                cut_db,
                time_sigma_ms: tab.tool_state.brush_time_radius_ms.max(1.0),
                freq_sigma_hz: tab.tool_state.brush_freq_radius_hz.max(1.0),
            });
        true
    }

    #[cfg(feature = "kittest")]
    pub fn test_spectral_brush_apply(&mut self) -> bool {
        let Some(tab_idx) = self.active_tab else {
            return false;
        };
        self.spawn_spectral_brush_apply_for_tab(tab_idx);
        self.editor_apply_state.is_some()
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

    fn brush_stamp(
        sample: usize,
        freq_hz: f32,
        cut_db: f32,
    ) -> crate::app::types::SpectralBrushStamp {
        crate::app::types::SpectralBrushStamp {
            sample,
            freq_hz,
            cut_db,
            time_sigma_ms: 80.0,
            freq_sigma_hz: 200.0,
        }
    }

    #[test]
    fn brush_no_stamps_is_identity() {
        let sr = 48_000u32;
        let sig = sine(1_000.0, sr, 24_000);
        let out = brush_channel_with_stamps(&sig, sr, &[]);
        assert_eq!(out, sig);
    }

    #[test]
    fn brush_attenuates_target_keeps_far_frequency() {
        let sr = 48_000u32;
        let len = 96_000usize; // 2 s
        let low = sine(1_000.0, sr, len);
        let high = sine(5_000.0, sr, len);
        let mixed: Vec<f32> = low.iter().zip(&high).map(|(a, b)| a + b).collect();
        let stamps = [brush_stamp(len / 2, 1_000.0, 30.0)];
        let out = brush_channel_with_stamps(&mixed, sr, &stamps);
        assert_eq!(out.len(), mixed.len());
        // Around the stamp (within ~1 sigma) the 1 kHz partial drops hard.
        let probe = &out[len / 2 - 2_000..len / 2 + 2_000];
        let src = &mixed[len / 2 - 2_000..len / 2 + 2_000];
        let cut = goertzel(probe, sr, 1_000.0);
        let cut_src = goertzel(src, sr, 1_000.0);
        let drop_db = 20.0 * (cut / cut_src.max(1e-12)).log10();
        assert!(drop_db < -15.0, "1 kHz not attenuated enough: {drop_db} dB");
        // 5 kHz (far outside the 200 Hz sigma) survives within 0.5 dB.
        let keep = goertzel(probe, sr, 5_000.0);
        let keep_src = goertzel(src, sr, 5_000.0);
        let keep_db = 20.0 * (keep / keep_src.max(1e-12)).log10();
        assert!(keep_db.abs() < 0.5, "5 kHz changed by {keep_db} dB");
    }

    #[test]
    fn brush_region_is_bounded() {
        let sr = 48_000u32;
        let len = 192_000usize; // 4 s
        let sig = sine(1_000.0, sr, len);
        let stamps = [brush_stamp(len / 2, 1_000.0, 24.0)];
        let out = brush_channel_with_stamps(&sig, sr, &stamps);
        let sigma = (80.0 / 1000.0) * sr as f32;
        let reach = (sigma * 3.0).ceil() as usize + SPECTRAL_FFT_SIZE * 2;
        let seg_s = len / 2 - reach;
        let seg_e = len / 2 + reach;
        assert_eq!(&out[..seg_s], &sig[..seg_s], "audio before the stamp changed");
        assert_eq!(&out[seg_e..], &sig[seg_e..], "audio after the stamp changed");
    }

    #[test]
    fn brush_stacking_deepens_cut_and_clamps() {
        let sr = 48_000u32;
        let len = 96_000usize;
        let sig = sine(1_000.0, sr, len);
        let one = [brush_stamp(len / 2, 1_000.0, 12.0)];
        let two = [
            brush_stamp(len / 2, 1_000.0, 12.0),
            brush_stamp(len / 2, 1_000.0, 12.0),
        ];
        let out1 = brush_channel_with_stamps(&sig, sr, &one);
        let out2 = brush_channel_with_stamps(&sig, sr, &two);
        let probe = len / 2 - 1_000..len / 2 + 1_000;
        let m1 = goertzel(&out1[probe.clone()], sr, 1_000.0);
        let m2 = goertzel(&out2[probe.clone()], sr, 1_000.0);
        assert!(m2 < m1 * 0.7, "stacked stamps did not deepen the cut: {m2} vs {m1}");
        // A huge stack clamps at MAX_BRUSH_CUT_DB instead of denormals.
        let many: Vec<_> = (0..20).map(|_| brush_stamp(len / 2, 1_000.0, 40.0)).collect();
        let out_many = brush_channel_with_stamps(&sig, sr, &many);
        assert!(out_many.iter().all(|v| v.is_finite()));
    }

    /// Deterministic LCG noise in [-1, 1].
    fn lcg_noise(len: usize, seed: u64) -> Vec<f32> {
        let mut state = seed.max(1);
        (0..len)
            .map(|_| {
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                ((state >> 33) as f32 / (u32::MAX >> 1) as f32) * 2.0 - 1.0
            })
            .collect()
    }

    #[test]
    fn denoise_reduces_noise_keeps_tone() {
        let sr = 48_000u32;
        let len = 96_000usize; // 2 s
        let noise_amp = 10f32.powf(-30.0 / 20.0);
        let tone_amp = 10f32.powf(-12.0 / 20.0);
        let noise: Vec<f32> = lcg_noise(len, 7).iter().map(|v| v * noise_amp).collect();
        // First half: noise only. Second half: noise + tone.
        let mut sig = noise.clone();
        for i in len / 2..len {
            sig[i] += (2.0 * core::f32::consts::PI * 1_000.0 * i as f32 / sr as f32).sin()
                * tone_amp;
        }
        // Learn on the noise-only first half.
        let profile = learn_noise_profile_channel(&sig, 4_800, len / 2 - 4_800);
        let out = denoise_channel(&sig, &profile, 20.0, 2.0, None);
        assert_eq!(out.len(), sig.len());
        // Noise-only region drops by at least 12 dB.
        let probe_noise = 10_000..len / 2 - 10_000;
        let before_rms = rms(&sig[probe_noise.clone()]);
        let after_rms = rms(&out[probe_noise.clone()]);
        let drop_db = 20.0 * (after_rms / before_rms.max(1e-12)).log10();
        assert!(drop_db < -12.0, "noise only dropped {drop_db} dB");
        // The tone survives within ~1 dB.
        let probe_tone = len / 2 + 10_000..len - 10_000;
        let tone_before = goertzel(&sig[probe_tone.clone()], sr, 1_000.0);
        let tone_after = goertzel(&out[probe_tone.clone()], sr, 1_000.0);
        let tone_db = 20.0 * (tone_after / tone_before.max(1e-12)).log10();
        assert!(tone_db.abs() < 1.0, "tone changed by {tone_db} dB");
    }

    #[test]
    fn denoise_reduction_floor_is_respected() {
        let sr = 48_000u32;
        let _ = sr;
        let len = 96_000usize;
        let noise: Vec<f32> = lcg_noise(len, 11)
            .iter()
            .map(|v| v * 10f32.powf(-30.0 / 20.0))
            .collect();
        let profile = learn_noise_profile_channel(&noise, 4_800, len - 4_800);
        // A 12 dB floor caps the attenuation: expect roughly 8..14 dB.
        let out = denoise_channel(&noise, &profile, 12.0, 2.0, None);
        let probe = 10_000..len - 10_000;
        let drop_db =
            20.0 * (rms(&out[probe.clone()]) / rms(&noise[probe.clone()]).max(1e-12)).log10();
        assert!(
            drop_db < -8.0 && drop_db > -14.0,
            "12 dB floor not respected: dropped {drop_db} dB"
        );
    }

    #[test]
    fn denoise_bad_profile_is_identity() {
        let sig = sine(1_000.0, 48_000, 24_000);
        // Wrong bin count -> untouched.
        let out = denoise_channel(&sig, &[0.1f32; 64], 20.0, 2.0, None);
        assert_eq!(out, sig);
        // Empty (all-zero) profile -> untouched.
        let bins = SPECTRAL_FFT_SIZE / 2 + 1;
        let out = denoise_channel(&sig, &vec![0.0f32; bins], 20.0, 2.0, None);
        assert_eq!(out, sig);
    }

    #[test]
    fn denoise_range_leaves_outside_bit_identical() {
        let len = 96_000usize;
        let noise: Vec<f32> = lcg_noise(len, 23)
            .iter()
            .map(|v| v * 10f32.powf(-30.0 / 20.0))
            .collect();
        let profile = learn_noise_profile_channel(&noise, 4_800, len / 2);
        let (s, e) = (len / 2, len / 2 + 19_200);
        let out = denoise_channel(&noise, &profile, 20.0, 2.0, Some((s, e)));
        let seg_s = s - SPECTRAL_FFT_SIZE * 2;
        let seg_e = e + SPECTRAL_FFT_SIZE * 2;
        assert_eq!(&out[..s], &noise[..s], "audio before the range changed");
        assert_eq!(&out[e..], &noise[e..], "audio after the range changed");
        let _ = (seg_s, seg_e);
        // Inside the range the noise actually drops.
        let probe = s + 4_800..e - 4_800;
        let drop_db =
            20.0 * (rms(&out[probe.clone()]) / rms(&noise[probe.clone()]).max(1e-12)).log10();
        assert!(drop_db < -8.0, "in-range noise only dropped {drop_db} dB");
    }

    #[test]
    fn heal_degenerate_selection_is_identity() {
        let sr = 48_000u32;
        let sig = sine(1_000.0, sr, 24_000);
        let out = heal_channel_range(&sig, sr, 12_000, 12_000, None, 100.0, 5.0);
        assert_eq!(out, sig);
        // Selection shorter than one hop: no interior frame, identity.
        let out = heal_channel_range(&sig, sr, 12_000, 12_100, None, 100.0, 5.0);
        assert_eq!(out, sig);
    }

    #[test]
    fn heal_bridges_zeroed_dropout_with_the_tone() {
        let sr = 48_000u32;
        let len = 96_000usize; // 2 s
        let clean = sine(1_000.0, sr, len);
        let mut damaged = clean.clone();
        // 50 ms dropout in the middle.
        let s = len / 2;
        let e = s + (sr as f32 * 0.05) as usize;
        for v in &mut damaged[s..e] {
            *v = 0.0;
        }
        // Heal a selection just around the dropout.
        let sel_s = s.saturating_sub(256);
        let sel_e = e + 256;
        let out = heal_channel_range(&damaged, sr, sel_s, sel_e, None, 100.0, 5.0);
        assert_eq!(out.len(), damaged.len());
        // Outside the selection: bit-identical.
        assert_eq!(&out[..sel_s], &damaged[..sel_s]);
        assert_eq!(&out[sel_e..], &damaged[sel_e..]);
        // Inside the dropout the 1 kHz tone is rebuilt to a solid fraction
        // of the clean amplitude.
        let healed_mag = goertzel(&out[s..e], sr, 1_000.0);
        let clean_mag = goertzel(&clean[s..e], sr, 1_000.0);
        assert!(
            (healed_mag - clean_mag).abs() < clean_mag * 0.25,
            "tone not rebuilt: healed {healed_mag} vs clean {clean_mag}"
        );
        let damaged_mag = goertzel(&damaged[s..e], sr, 1_000.0);
        assert!(healed_mag > damaged_mag * 2.0, "heal changed nothing");
    }

    #[test]
    fn heal_removes_impulses_toward_the_tone() {
        let sr = 48_000u32;
        let len = 96_000usize;
        let clean = sine(500.0, sr, len);
        let mut damaged = clean.clone();
        let s = len / 2;
        let e = s + 4_800; // 100 ms
        // Scatter hard clicks through the region.
        let mut i = s + 37;
        while i < e {
            damaged[i] = 1.0;
            i += 331;
        }
        let out = heal_channel_range(&damaged, sr, s, e, None, 100.0, 5.0);
        // Interior (away from the edge fades) is close to the clean sine.
        let interior = s + 480..e - 480;
        let err: Vec<f32> = out[interior.clone()]
            .iter()
            .zip(&clean[interior])
            .map(|(o, c)| o - c)
            .collect();
        assert!(
            rms(&err) < 0.05,
            "healed interior deviates from clean tone: rms {}",
            rms(&err)
        );
    }

    #[test]
    fn heal_band_limited_keeps_out_of_band_content() {
        let sr = 48_000u32;
        let len = 96_000usize;
        let low = sine(440.0, sr, len);
        let high = sine(2_000.0, sr, len);
        let mixed: Vec<f32> = low.iter().zip(&high).map(|(a, b)| a + b).collect();
        let s = len / 2;
        let e = s + 4_800;
        // Heal only 1.5-2.5 kHz over a clean signal: 440 Hz must ride
        // through untouched, 2 kHz is rebuilt from context (~same tone).
        let out = heal_channel_range(&mixed, sr, s, e, Some((1_500.0, 2_500.0)), 100.0, 5.0);
        let probe = s + 480..e - 480;
        let low_out = goertzel(&out[probe.clone()], sr, 440.0);
        let low_src = goertzel(&mixed[probe.clone()], sr, 440.0);
        assert!(
            (low_out - low_src).abs() < low_src * 0.05,
            "440 Hz changed by band-limited heal: {low_out} vs {low_src}"
        );
        let high_out = goertzel(&out[probe.clone()], sr, 2_000.0);
        let high_src = goertzel(&mixed[probe.clone()], sr, 2_000.0);
        assert!(
            (high_out - high_src).abs() < high_src * 0.3,
            "2 kHz not rebuilt to context level: {high_out} vs {high_src}"
        );
        assert_eq!(&out[..s], &mixed[..s]);
        assert_eq!(&out[e..], &mixed[e..]);
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
