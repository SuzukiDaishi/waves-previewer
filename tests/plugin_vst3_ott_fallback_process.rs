use std::path::Path;
use std::path::PathBuf;

use neowaves::plugin::{PluginParamValue, WorkerRequest, WorkerResponse};

fn unique_temp_dir(tag: &str) -> PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("neowaves_{tag}_{stamp}"));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn write_test_wav(path: &Path) {
    let sr = 48_000u32;
    let len = 48_000usize;
    let mut ch = vec![0.0f32; len];
    for (i, v) in ch.iter_mut().enumerate() {
        let t = i as f32 / sr as f32;
        *v = (2.0 * std::f32::consts::PI * 220.0 * t).sin() * 0.2;
    }
    neowaves::wave::export_channels_audio(&[ch], sr, path).expect("write test wav");
}

fn mean_abs_diff(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len().min(b.len()).max(1);
    let mut acc = 0.0f32;
    for i in 0..n {
        acc += (a[i] - b[i]).abs();
    }
    acc / n as f32
}

fn peak_abs(samples: &[f32]) -> f32 {
    let mut peak = 0.0f32;
    for &v in samples {
        peak = peak.max(v.abs());
    }
    peak
}

#[test]
fn generic_fallback_uses_ott_signature_params() {
    let dir = unique_temp_dir("plugin_ott_fallback");
    let input = dir.join("in.wav");
    let output = dir.join("out.wav");
    write_test_wav(&input);

    let req = WorkerRequest::ProcessFx {
        plugin_path: "C:\\nope\\OTT.vst3".to_string(),
        input_audio_path: input.to_string_lossy().to_string(),
        output_audio_path: output.to_string_lossy().to_string(),
        sample_rate: 48_000,
        max_block_size: 1024,
        enabled: true,
        bypass: false,
        state_blob_b64: None,
        params: vec![
            PluginParamValue {
                id: "vst3:00000000".to_string(), // Depth
                normalized: 1.0,
            },
            PluginParamValue {
                id: "vst3:00000002".to_string(), // In Gain
                normalized: 0.85,
            },
            PluginParamValue {
                id: "vst3:00000003".to_string(), // Out Gain
                normalized: 0.35,
            },
            PluginParamValue {
                id: "vst3:00000013".to_string(), // Bypass
                normalized: 0.0,
            },
        ],
    };
    let resp = neowaves::plugin::worker::handle_request(req);
    match resp {
        WorkerResponse::ProcessResult { .. } => {}
        other => panic!("unexpected response: {other:?}"),
    }

    let (in_ch, _sr) = neowaves::audio_io::decode_audio_multi(&input).expect("decode input");
    let (out_ch, _sr2) = neowaves::audio_io::decode_audio_multi(&output).expect("decode output");
    assert_eq!(in_ch.len(), out_ch.len());
    let diff = mean_abs_diff(&in_ch[0], &out_ch[0]);
    assert!(
        diff > 0.001,
        "fallback should alter waveform for ott-like params, diff={diff}"
    );
    let peak = peak_abs(&out_ch[0]);
    assert!(
        peak <= 0.981,
        "fallback output should stay under limiter ceiling, peak={peak}"
    );

    let _ = std::fs::remove_dir_all(dir);
}
