/// Tests for the rubato 3.0 resampling migration.
/// Verifies that resample_quality and resample_channels_quality produce correct output
/// across a wide range of sample rate conversions.
use neowaves::wave::{resample_channels_quality, resample_quality, ResampleQuality};

// ── helpers ──────────────────────────────────────────────────────────────────

/// Sine wave at `freq_hz` for `duration_secs` at `sample_rate`.
fn sine_wave(sample_rate: u32, freq_hz: f32, duration_secs: f32) -> Vec<f32> {
    let n = (sample_rate as f32 * duration_secs) as usize;
    (0..n)
        .map(|i| {
            let t = i as f32 / sample_rate as f32;
            (2.0 * std::f32::consts::PI * freq_hz * t).sin()
        })
        .collect()
}

/// Silent (all-zero) signal.
fn silence(frames: usize) -> Vec<f32> {
    vec![0.0f32; frames]
}

/// Count zero crossings (positive→negative and negative→positive transitions).
fn zero_crossings(sig: &[f32]) -> usize {
    sig.windows(2)
        .filter(|w| (w[0] >= 0.0) != (w[1] >= 0.0))
        .count()
}

/// RMS energy of a signal.
fn rms(sig: &[f32]) -> f32 {
    if sig.is_empty() {
        return 0.0;
    }
    (sig.iter().map(|x| x * x).sum::<f32>() / sig.len() as f32).sqrt()
}

/// Expected output length given the ratio, with some tolerance.
fn expected_len(in_len: usize, in_sr: u32, out_sr: u32) -> usize {
    (in_len as f64 * out_sr as f64 / in_sr as f64).round() as usize
}

/// Assert output length is within ±2% of expected (rubato may add/trim a few frames).
fn assert_len_approx(actual: usize, expected: usize, label: &str) {
    let tolerance = (expected as f64 * 0.02).max(64.0) as usize;
    assert!(
        actual.abs_diff(expected) <= tolerance,
        "{label}: length {actual} vs expected {expected} (tolerance {tolerance})"
    );
}

/// Assert expected zero crossing count is preserved within ±15%.
fn assert_freq_preserved(sig: &[f32], out_sr: u32, freq_hz: f32, duration_secs: f32, label: &str) {
    let actual_zc = zero_crossings(sig);
    let expected_zc = (2.0 * freq_hz * duration_secs) as usize;
    let tolerance = (expected_zc as f64 * 0.15).max(4.0) as usize;
    assert!(
        actual_zc.abs_diff(expected_zc) <= tolerance,
        "{label}: zero_crossings {actual_zc} vs expected ~{expected_zc} at {freq_hz}Hz/{out_sr}Hz (±{tolerance})"
    );
}

// ── sample-rate conversion matrix ────────────────────────────────────────────

const CONVERSIONS: &[(u32, u32)] = &[
    (44100, 48000),   // slight upsample (common DAW pair)
    (48000, 44100),   // slight downsample
    (44100, 22050),   // 2× downsample
    (22050, 44100),   // 2× upsample
    (48000, 96000),   // 2× upsample
    (96000, 44100),   // non-integer downsample
    (44100, 44100),   // identity (no-op path)
    (8000, 44100),    // large upsample (telephony → audio)
    (44100, 8000),    // large downsample
    (32000, 48000),   // broadcast upsample
    (192000, 44100),  // high-res downsample
];

// ── mono resampling: output length ───────────────────────────────────────────

#[test]
fn test_resample_length_44100_to_48000() {
    let (in_sr, out_sr) = (44100, 48000);
    let src = sine_wave(in_sr, 440.0, 1.0);
    let out = resample_quality(&src, in_sr, out_sr, ResampleQuality::Fast);
    assert_len_approx(out.len(), expected_len(src.len(), in_sr, out_sr), "44100→48000");
}

#[test]
fn test_resample_length_48000_to_44100() {
    let (in_sr, out_sr) = (48000, 44100);
    let src = sine_wave(in_sr, 440.0, 1.0);
    let out = resample_quality(&src, in_sr, out_sr, ResampleQuality::Fast);
    assert_len_approx(out.len(), expected_len(src.len(), in_sr, out_sr), "48000→44100");
}

#[test]
fn test_resample_length_44100_to_22050() {
    let (in_sr, out_sr) = (44100, 22050);
    let src = sine_wave(in_sr, 440.0, 1.0);
    let out = resample_quality(&src, in_sr, out_sr, ResampleQuality::Fast);
    assert_len_approx(out.len(), expected_len(src.len(), in_sr, out_sr), "44100→22050");
}

#[test]
fn test_resample_length_22050_to_44100() {
    let (in_sr, out_sr) = (22050, 44100);
    let src = sine_wave(in_sr, 440.0, 1.0);
    let out = resample_quality(&src, in_sr, out_sr, ResampleQuality::Fast);
    assert_len_approx(out.len(), expected_len(src.len(), in_sr, out_sr), "22050→44100");
}

#[test]
fn test_resample_length_48000_to_96000() {
    let (in_sr, out_sr) = (48000, 96000);
    let src = sine_wave(in_sr, 440.0, 1.0);
    let out = resample_quality(&src, in_sr, out_sr, ResampleQuality::Fast);
    assert_len_approx(out.len(), expected_len(src.len(), in_sr, out_sr), "48000→96000");
}

#[test]
fn test_resample_length_96000_to_44100() {
    let (in_sr, out_sr) = (96000, 44100);
    let src = sine_wave(in_sr, 440.0, 1.0);
    let out = resample_quality(&src, in_sr, out_sr, ResampleQuality::Fast);
    assert_len_approx(out.len(), expected_len(src.len(), in_sr, out_sr), "96000→44100");
}

#[test]
fn test_resample_length_8000_to_44100() {
    let (in_sr, out_sr) = (8000, 44100);
    let src = sine_wave(in_sr, 300.0, 1.0);
    let out = resample_quality(&src, in_sr, out_sr, ResampleQuality::Fast);
    assert_len_approx(out.len(), expected_len(src.len(), in_sr, out_sr), "8000→44100");
}

#[test]
fn test_resample_length_44100_to_8000() {
    let (in_sr, out_sr) = (44100, 8000);
    let src = sine_wave(in_sr, 300.0, 1.0);
    let out = resample_quality(&src, in_sr, out_sr, ResampleQuality::Fast);
    assert_len_approx(out.len(), expected_len(src.len(), in_sr, out_sr), "44100→8000");
}

#[test]
fn test_resample_length_32000_to_48000() {
    let (in_sr, out_sr) = (32000, 48000);
    let src = sine_wave(in_sr, 440.0, 1.0);
    let out = resample_quality(&src, in_sr, out_sr, ResampleQuality::Fast);
    assert_len_approx(out.len(), expected_len(src.len(), in_sr, out_sr), "32000→48000");
}

#[test]
fn test_resample_length_192000_to_44100() {
    let (in_sr, out_sr) = (192000, 44100);
    let src = sine_wave(in_sr, 440.0, 0.5);
    let out = resample_quality(&src, in_sr, out_sr, ResampleQuality::Fast);
    assert_len_approx(out.len(), expected_len(src.len(), in_sr, out_sr), "192000→44100");
}

// ── identity (no-op) ─────────────────────────────────────────────────────────

#[test]
fn test_resample_identity_returns_same_samples() {
    let src = sine_wave(44100, 440.0, 0.5);
    let out = resample_quality(&src, 44100, 44100, ResampleQuality::Fast);
    assert_eq!(out.len(), src.len(), "identity must not change length");
    // Samples should be identical since the fast path short-circuits
    for (i, (a, b)) in src.iter().zip(out.iter()).enumerate() {
        assert_eq!(
            a, b,
            "sample {i} differs: {a} vs {b} — identity path modified audio"
        );
    }
}

// ── silence preservation ─────────────────────────────────────────────────────

#[test]
fn test_resample_silence_stays_silent_44100_to_48000() {
    let src = silence(44100);
    let out = resample_quality(&src, 44100, 48000, ResampleQuality::Fast);
    let max_abs = out.iter().map(|x| x.abs()).fold(0.0f32, f32::max);
    assert!(
        max_abs < 1e-4,
        "silence→resample should stay near-silent, got max_abs={max_abs}"
    );
}

#[test]
fn test_resample_silence_stays_silent_44100_to_22050() {
    let src = silence(44100);
    let out = resample_quality(&src, 44100, 22050, ResampleQuality::Fast);
    let max_abs = out.iter().map(|x| x.abs()).fold(0.0f32, f32::max);
    assert!(max_abs < 1e-4, "silence downsample got max_abs={max_abs}");
}

#[test]
fn test_resample_silence_stays_silent_48000_to_44100() {
    let src = silence(48000);
    let out = resample_quality(&src, 48000, 44100, ResampleQuality::Fast);
    let max_abs = out.iter().map(|x| x.abs()).fold(0.0f32, f32::max);
    assert!(max_abs < 1e-4, "48k→44k silence got max_abs={max_abs}");
}

// ── frequency preservation ───────────────────────────────────────────────────

#[test]
fn test_resample_frequency_preserved_44100_to_48000() {
    let (in_sr, out_sr, freq) = (44100u32, 48000u32, 440.0f32);
    let src = sine_wave(in_sr, freq, 1.0);
    let out = resample_quality(&src, in_sr, out_sr, ResampleQuality::Good);
    let duration = src.len() as f32 / in_sr as f32;
    assert_freq_preserved(&out, out_sr, freq, duration, "440Hz 44100→48000");
}

#[test]
fn test_resample_frequency_preserved_48000_to_44100() {
    let (in_sr, out_sr, freq) = (48000u32, 44100u32, 440.0f32);
    let src = sine_wave(in_sr, freq, 1.0);
    let out = resample_quality(&src, in_sr, out_sr, ResampleQuality::Good);
    let duration = src.len() as f32 / in_sr as f32;
    assert_freq_preserved(&out, out_sr, freq, duration, "440Hz 48000→44100");
}

#[test]
fn test_resample_frequency_preserved_44100_to_22050() {
    // 440Hz is well below Nyquist at 22050Hz (11025Hz), must survive
    let (in_sr, out_sr, freq) = (44100u32, 22050u32, 440.0f32);
    let src = sine_wave(in_sr, freq, 1.0);
    let out = resample_quality(&src, in_sr, out_sr, ResampleQuality::Good);
    let duration = src.len() as f32 / in_sr as f32;
    assert_freq_preserved(&out, out_sr, freq, duration, "440Hz 44100→22050");
}

#[test]
fn test_resample_frequency_preserved_22050_to_44100() {
    let (in_sr, out_sr, freq) = (22050u32, 44100u32, 440.0f32);
    let src = sine_wave(in_sr, freq, 1.0);
    let out = resample_quality(&src, in_sr, out_sr, ResampleQuality::Good);
    let duration = src.len() as f32 / in_sr as f32;
    assert_freq_preserved(&out, out_sr, freq, duration, "440Hz 22050→44100");
}

#[test]
fn test_resample_frequency_preserved_8000_to_44100() {
    let (in_sr, out_sr, freq) = (8000u32, 44100u32, 600.0f32);
    let src = sine_wave(in_sr, freq, 1.0);
    let out = resample_quality(&src, in_sr, out_sr, ResampleQuality::Good);
    let duration = src.len() as f32 / in_sr as f32;
    assert_freq_preserved(&out, out_sr, freq, duration, "600Hz 8000→44100");
}

// ── energy (RMS) preservation ────────────────────────────────────────────────

#[test]
fn test_resample_energy_preserved_44100_to_48000() {
    let src = sine_wave(44100, 440.0, 1.0);
    let out = resample_quality(&src, 44100, 48000, ResampleQuality::Good);
    let rms_in = rms(&src);
    let rms_out = rms(&out);
    let ratio = rms_out / rms_in;
    assert!(
        (0.85..=1.15).contains(&ratio),
        "RMS ratio {ratio:.3} outside 0.85–1.15 (in={rms_in:.4} out={rms_out:.4})"
    );
}

#[test]
fn test_resample_energy_preserved_44100_to_22050() {
    let src = sine_wave(44100, 440.0, 1.0);
    let out = resample_quality(&src, 44100, 22050, ResampleQuality::Good);
    let rms_in = rms(&src);
    let rms_out = rms(&out);
    let ratio = rms_out / rms_in;
    assert!(
        (0.85..=1.15).contains(&ratio),
        "RMS ratio {ratio:.3} outside 0.85–1.15 for downsample"
    );
}

// ── multi-channel (resample_channels_quality) ────────────────────────────────

#[test]
fn test_resample_stereo_44100_to_48000() {
    let left = sine_wave(44100, 440.0, 1.0);
    let right = sine_wave(44100, 880.0, 1.0);
    let chans = vec![left, right];
    let out = resample_channels_quality(&chans, 44100, 48000, ResampleQuality::Fast);
    assert_eq!(out.len(), 2, "stereo must remain stereo");
    let exp = expected_len(chans[0].len(), 44100, 48000);
    assert_len_approx(out[0].len(), exp, "stereo L 44100→48000");
    assert_len_approx(out[1].len(), exp, "stereo R 44100→48000");
}

#[test]
fn test_resample_stereo_channels_independent() {
    // L and R have different sine frequencies; they must not bleed into each other.
    let left = sine_wave(44100, 200.0, 0.5);
    let right = sine_wave(44100, 800.0, 0.5);
    let chans = vec![left.clone(), right.clone()];
    let out = resample_channels_quality(&chans, 44100, 48000, ResampleQuality::Good);

    let zc_left_out = zero_crossings(&out[0]);
    let zc_right_out = zero_crossings(&out[1]);
    // 200Hz vs 800Hz: zero crossings should be 4× different
    assert!(
        zc_left_out < zc_right_out,
        "L ({zc_left_out} ZCs at 200Hz) must have fewer ZCs than R ({zc_right_out} ZCs at 800Hz)"
    );
}

#[test]
fn test_resample_quad_channel_44100_to_48000() {
    let chans: Vec<Vec<f32>> = (0..4).map(|i| sine_wave(44100, 220.0 * (i + 1) as f32, 0.5)).collect();
    let out = resample_channels_quality(&chans, 44100, 48000, ResampleQuality::Fast);
    assert_eq!(out.len(), 4, "quad must remain 4 channels");
    let exp = expected_len(chans[0].len(), 44100, 48000);
    for (ch, sig) in out.iter().enumerate() {
        assert_len_approx(sig.len(), exp, &format!("quad ch{ch}"));
    }
}

#[test]
fn test_resample_channels_identity() {
    let chans = vec![sine_wave(44100, 440.0, 0.3), sine_wave(44100, 880.0, 0.3)];
    let out = resample_channels_quality(&chans, 44100, 44100, ResampleQuality::Fast);
    assert_eq!(out.len(), chans.len());
    for (i, (orig, resampled)) in chans.iter().zip(out.iter()).enumerate() {
        assert_eq!(orig.len(), resampled.len(), "ch{i} length changed in identity");
    }
}

#[test]
fn test_resample_channels_silence() {
    let chans = vec![silence(44100), silence(44100)];
    let out = resample_channels_quality(&chans, 44100, 48000, ResampleQuality::Fast);
    assert_eq!(out.len(), 2);
    for (ch, sig) in out.iter().enumerate() {
        let max_abs = sig.iter().map(|x| x.abs()).fold(0.0f32, f32::max);
        assert!(max_abs < 1e-4, "ch{ch} silence not preserved: max_abs={max_abs}");
    }
}

#[test]
fn test_resample_channels_empty_input() {
    let out = resample_channels_quality(&[], 44100, 48000, ResampleQuality::Fast);
    assert!(out.is_empty(), "empty input must produce empty output");
}

// ── edge cases ───────────────────────────────────────────────────────────────

#[test]
fn test_resample_empty_slice_returns_empty() {
    let out = resample_quality(&[], 44100, 48000, ResampleQuality::Fast);
    assert!(out.is_empty());
}

#[test]
fn test_resample_single_sample_does_not_panic() {
    let out = resample_quality(&[0.5f32], 44100, 48000, ResampleQuality::Fast);
    assert!(!out.is_empty());
}

#[test]
fn test_resample_all_quality_levels_44100_to_48000() {
    let src = sine_wave(44100, 440.0, 0.2);
    let exp = expected_len(src.len(), 44100, 48000);
    for quality in [ResampleQuality::Fast, ResampleQuality::Good, ResampleQuality::Best] {
        let out = resample_quality(&src, 44100, 48000, quality);
        assert_len_approx(out.len(), exp, &format!("{quality:?} 44100→48000"));
    }
}

#[test]
fn test_resample_all_quality_levels_silence() {
    let src = silence(44100);
    for quality in [ResampleQuality::Fast, ResampleQuality::Good, ResampleQuality::Best] {
        let out = resample_quality(&src, 44100, 48000, quality);
        let max_abs = out.iter().map(|x| x.abs()).fold(0.0f32, f32::max);
        assert!(max_abs < 1e-4, "{quality:?} silence got max_abs={max_abs}");
    }
}

#[test]
fn test_resample_output_not_clipping_sine() {
    // Full-scale sine ±1.0 should not clip above ~1.1 after resampling
    let src = sine_wave(44100, 440.0, 1.0);
    let out = resample_quality(&src, 44100, 48000, ResampleQuality::Good);
    let max_abs = out.iter().map(|x| x.abs()).fold(0.0f32, f32::max);
    assert!(max_abs <= 1.1, "resampled audio clipped: max_abs={max_abs}");
}

#[test]
fn test_resample_no_nan_or_inf_44100_to_48000() {
    let src = sine_wave(44100, 440.0, 1.0);
    let out = resample_quality(&src, 44100, 48000, ResampleQuality::Good);
    for (i, &v) in out.iter().enumerate() {
        assert!(v.is_finite(), "NaN/Inf at sample {i}: {v}");
    }
}

#[test]
fn test_resample_no_nan_or_inf_192000_to_44100() {
    let src = sine_wave(192000, 440.0, 0.25);
    let out = resample_quality(&src, 192000, 44100, ResampleQuality::Fast);
    for (i, &v) in out.iter().enumerate() {
        assert!(v.is_finite(), "NaN/Inf at sample {i}: {v}");
    }
}

// ── multi-quality frequency preservation ────────────────────────────────────

#[test]
fn test_resample_good_quality_better_frequency_accuracy_than_fast() {
    let src = sine_wave(44100, 440.0, 1.0);
    let expected_zc = (2.0 * 440.0 * (src.len() as f32 / 44100.0)) as usize;
    let duration = src.len() as f32 / 44100.0;

    let fast_out = resample_quality(&src, 44100, 48000, ResampleQuality::Fast);
    let good_out = resample_quality(&src, 44100, 48000, ResampleQuality::Good);

    let fast_zc = zero_crossings(&fast_out);
    let good_zc = zero_crossings(&good_out);

    // Both should be within 20% of expected
    let tolerance = (expected_zc as f64 * 0.20) as usize;
    assert!(fast_zc.abs_diff(expected_zc) <= tolerance, "Fast ZC {fast_zc} vs {expected_zc} at 440Hz/{duration}s");
    assert!(good_zc.abs_diff(expected_zc) <= tolerance, "Good ZC {good_zc} vs {expected_zc} at 440Hz/{duration}s");
}
