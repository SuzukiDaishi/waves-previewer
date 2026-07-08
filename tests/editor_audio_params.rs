// Tests for the editor inspector audio-parameter DSP primitives:
// gain envelope (automation curve), crossfaded range splice used by
// Speed / TimeStretch / PitchShift selection edits, and range reverse.

use neowaves::wave::{
    apply_gain_envelope_in_place, gain_envelope_db_at, process_speed_offline,
    reverse_range_with_crossfade, splice_range_with_crossfade, splice_xfade_samples,
};

fn ramp(len: usize) -> Vec<f32> {
    (0..len).map(|i| i as f32 / len as f32).collect()
}

#[test]
fn gain_envelope_empty_points_uses_fallback() {
    let mut buf = vec![0.5f32; 100];
    apply_gain_envelope_in_place(&mut buf, &[], -6.0, false);
    let expected = 0.5 * 10f32.powf(-6.0 / 20.0);
    for &v in &buf {
        assert!((v - expected).abs() < 1e-6);
    }
}

#[test]
fn gain_envelope_head_tail_extend_flat() {
    // One point at sample 50 with +6 dB: whole buffer gets +6 dB.
    let mut buf = vec![0.25f32; 100];
    apply_gain_envelope_in_place(&mut buf, &[(50, 6.0)], 0.0, false);
    let expected = 0.25 * 10f32.powf(6.0 / 20.0);
    for &v in &buf {
        assert!((v - expected).abs() < 1e-6, "{v} vs {expected}");
    }
}

#[test]
fn gain_envelope_interpolates_in_db() {
    // Two points: 0 dB at 0, -12 dB at 100. Midpoint should be -6 dB.
    let pts = [(0usize, 0.0f32), (100usize, -12.0f32)];
    let mid_db = gain_envelope_db_at(&pts, 0.0, 50);
    assert!((mid_db + 6.0).abs() < 1e-4, "mid {mid_db}");
    let mut buf = vec![1.0f32; 101];
    apply_gain_envelope_in_place(&mut buf, &pts, 0.0, false);
    assert!((buf[0] - 1.0).abs() < 1e-5);
    let expected_mid = 10f32.powf(-6.0 / 20.0);
    assert!((buf[50] - expected_mid).abs() < 1e-3, "{}", buf[50]);
    // Tail (>= last point) is flat at -12 dB.
    let expected_tail = 10f32.powf(-12.0 / 20.0);
    assert!((buf[100] - expected_tail).abs() < 1e-4);
}

#[test]
fn gain_envelope_unsorted_points_are_sorted() {
    let pts = [(100usize, -12.0f32), (0usize, 0.0f32)];
    let db = gain_envelope_db_at(&pts, 0.0, 50);
    assert!((db + 6.0).abs() < 1e-4);
}

#[test]
fn gain_envelope_apply_clamps_output() {
    let mut buf = vec![0.9f32; 10];
    apply_gain_envelope_in_place(&mut buf, &[(0, 24.0)], 0.0, true);
    for &v in &buf {
        assert!(v <= 1.0);
    }
}

#[test]
fn splice_replaces_range_and_changes_length() {
    let base = ramp(1000);
    // Replace 400 samples with 200 (selection sped up 2x).
    let processed = vec![0.5f32; 200];
    let out = splice_range_with_crossfade(&base, 300, 700, &processed, 32);
    assert_eq!(out.len(), 1000 - 400 + 200);
    // Prefix and suffix are untouched.
    assert_eq!(&out[..300], &base[..300]);
    assert_eq!(&out[500..], &base[700..]);
}

#[test]
fn splice_grows_range() {
    let base = ramp(1000);
    let processed = vec![0.25f32; 800];
    let out = splice_range_with_crossfade(&base, 300, 700, &processed, 32);
    assert_eq!(out.len(), 1000 - 400 + 800);
    assert_eq!(&out[..300], &base[..300]);
    assert_eq!(&out[1100..], &base[700..]);
}

#[test]
fn splice_head_join_is_continuous() {
    // Original is a smooth ramp; processed segment is constant 1.0 (a hard
    // discontinuity without a crossfade). With the crossfade, the first
    // samples of the spliced segment must stay near the original ramp.
    let base = ramp(1000);
    let processed = vec![1.0f32; 400];
    let xf = 64;
    let out = splice_range_with_crossfade(&base, 300, 700, &processed, xf);
    // Just after the join, the value must be close to the original (~0.3),
    // not the processed constant 1.0.
    let jump = (out[300] - base[300]).abs();
    assert!(jump < 0.05, "head join jump too large: {jump}");
    // And just before the suffix, close to the original selection end (~0.7).
    let tail_idx = 300 + 400 - 1;
    let jump_tail = (out[tail_idx] - base[699]).abs();
    assert!(jump_tail < 0.05, "tail join jump too large: {jump_tail}");
}

#[test]
fn splice_whole_buffer_without_neighbors_keeps_processed() {
    // No prefix/suffix: no blending is applied at buffer boundaries.
    let base = vec![0.0f32; 100];
    let processed = vec![1.0f32; 50];
    let out = splice_range_with_crossfade(&base, 0, 100, &processed, 16);
    assert_eq!(out, processed);
}

#[test]
fn splice_xfade_bounded_by_segments() {
    assert_eq!(splice_xfade_samples(48_000, 10_000, 10_000), 384); // 8 ms
    assert_eq!(splice_xfade_samples(48_000, 100, 10_000), 50); // half selection
    assert_eq!(splice_xfade_samples(48_000, 10_000, 60), 30); // half processed
}

#[test]
fn reverse_full_buffer_is_plain_reverse() {
    let base = ramp(100);
    let mut buf = base.clone();
    reverse_range_with_crossfade(&mut buf, 0, 100, 16);
    let mut expected = base.clone();
    expected.reverse();
    assert_eq!(buf, expected);
}

#[test]
fn reverse_subrange_only_touches_selection_and_smooths_joins() {
    let base = ramp(1000);
    let mut buf = base.clone();
    reverse_range_with_crossfade(&mut buf, 300, 700, 64);
    assert_eq!(&buf[..300], &base[..300]);
    assert_eq!(&buf[700..], &base[700..]);
    // Center of the selection is fully reversed.
    assert!((buf[500] - base[499]).abs() < 2e-3);
    // Joins stay continuous: the first/last samples of the reversed span are
    // blended toward the original values.
    assert!((buf[300] - base[300]).abs() < 0.05, "head {}", buf[300]);
    assert!((buf[699] - base[699]).abs() < 0.05, "tail {}", buf[699]);
}

#[test]
fn speed_offline_changes_length_inverse_to_rate() {
    let base = ramp(1000);
    let faster = process_speed_offline(&base, 2.0);
    assert_eq!(faster.len(), 500);
    let slower = process_speed_offline(&base, 0.5);
    assert_eq!(slower.len(), 2000);
    let same = process_speed_offline(&base, 1.0);
    assert_eq!(same.len(), 1000);
}

#[test]
fn speed_selection_splice_length_math() {
    // Simulate the Speed-on-selection apply: process the selection, splice it
    // back, and check the final length.
    let base = ramp(2000);
    let (s, e) = (500usize, 1500usize);
    let processed = process_speed_offline(&base[s..e], 2.0);
    let xf = splice_xfade_samples(48_000, e - s, processed.len());
    let out = splice_range_with_crossfade(&base, s, e, &processed, xf);
    assert_eq!(out.len(), 2000 - 1000 + 500);
    assert_eq!(&out[..s], &base[..s]);
}
