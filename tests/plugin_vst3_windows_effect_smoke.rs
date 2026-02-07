#![cfg(windows)]

use std::path::Path;
use std::path::PathBuf;

use neowaves::plugin::{PluginHostBackend, PluginParamInfo, PluginParamValue, WorkerRequest, WorkerResponse};

const VST3_PATHS: &[&str] = &[
    r"C:\Program Files\Common Files\VST3\OTT.vst3",
    r"C:\Program Files\Common Files\VST3\Colourizer Rs.vst3",
    r"C:\Program Files\Common Files\VST3\Condenser Rs.vst3",
    r"C:\Program Files\Common Files\VST3\Diffuser Plugin.vst3",
];

fn unique_temp_dir(tag: &str) -> PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("neowaves_{tag}_{stamp}"));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn write_stereo_test_wav(path: &Path) {
    let sr = 48_000u32;
    let len = sr as usize * 3;
    let mut l = vec![0.0f32; len];
    let mut r = vec![0.0f32; len];
    for i in 0..len {
        let t = i as f32 / sr as f32;
        let env = (1.0 - (t / 3.0)).max(0.1);
        l[i] = (2.0 * std::f32::consts::PI * 180.0 * t).sin() * 0.24 * env;
        r[i] = (2.0 * std::f32::consts::PI * 360.0 * t).sin() * 0.20 * env;
    }
    neowaves::wave::export_channels_audio(&[l, r], sr, path).expect("write test wav");
}

fn mean_abs_diff(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len().min(b.len()).max(1);
    let mut acc = 0.0f32;
    for i in 0..n {
        acc += (a[i] - b[i]).abs();
    }
    acc / n as f32
}

fn all_finite(chs: &[Vec<f32>]) -> bool {
    chs.iter().all(|ch| ch.iter().all(|v| v.is_finite()))
}

fn plugin_keywords(path: &str) -> &'static [&'static str] {
    let low = path.to_ascii_lowercase();
    if low.contains("ott") {
        &["depth", "mix", "upwd", "dnwd", "strength", "gain", "amount"]
    } else if low.contains("colourizer") || low.contains("colorizer") {
        &["color", "colour", "saturation", "drive", "mix", "amount", "gain"]
    } else if low.contains("condenser") {
        &["threshold", "ratio", "mix", "amount", "attack", "release", "gain"]
    } else if low.contains("diffuser") {
        &["diffusion", "size", "feedback", "mix", "amount", "gain", "time"]
    } else {
        &["mix", "amount", "gain", "depth"]
    }
}

fn choose_test_params(plugin_path: &str, params: &[PluginParamInfo]) -> Vec<PluginParamValue> {
    let mut out: Vec<PluginParamValue> = params
        .iter()
        .map(|p| PluginParamValue {
            id: p.id.clone(),
            normalized: p.default_normalized.clamp(0.0, 1.0),
        })
        .collect();

    for (i, info) in params.iter().enumerate() {
        if info.name.to_ascii_lowercase().contains("bypass") {
            out[i].normalized = 0.0;
        }
    }

    let keywords = plugin_keywords(plugin_path);
    if let Some((i, _)) = params.iter().enumerate().find(|(_, p)| {
        let name = p.name.to_ascii_lowercase();
        keywords.iter().any(|k| name.contains(k))
    }) {
        let cur = out[i].normalized;
        out[i].normalized = if cur < 0.75 { 1.0 } else { 0.0 };
    }
    out
}

#[test]
fn named_vst3_plugins_native_process_smoke() {
    let dir = unique_temp_dir("plugin_vst3_smoke_named");
    let input = dir.join("in.wav");
    write_stereo_test_wav(&input);
    let (in_ch, _sr) = neowaves::audio_io::decode_audio_multi(&input).expect("decode input");

    let mut checked = 0usize;
    for plugin_path in VST3_PATHS {
        if !Path::new(plugin_path).exists() {
            eprintln!("skip: missing {}", plugin_path);
            continue;
        }
        checked += 1;

        let probe = neowaves::plugin::client::run_request(&WorkerRequest::Probe {
            plugin_path: (*plugin_path).to_string(),
        })
        .expect("probe response");
        let params = match probe {
            WorkerResponse::ProbeResult { backend, params, .. } => {
                assert_eq!(
                    backend,
                    PluginHostBackend::NativeVst3,
                    "probe should use native backend for {}",
                    plugin_path
                );
                assert!(
                    !params.is_empty(),
                    "probe should expose parameters for {}",
                    plugin_path
                );
                params
            }
            other => panic!("unexpected probe response for {}: {:?}", plugin_path, other),
        };

        let output = dir.join(format!("{}.wav", checked));
        let req = WorkerRequest::ProcessFx {
            plugin_path: (*plugin_path).to_string(),
            input_audio_path: input.to_string_lossy().to_string(),
            output_audio_path: output.to_string_lossy().to_string(),
            sample_rate: 48_000,
            max_block_size: 1024,
            enabled: true,
            bypass: false,
            state_blob_b64: None,
            params: choose_test_params(plugin_path, &params),
        };
        let resp = neowaves::plugin::client::run_request(&req).expect("process response");
        match resp {
            WorkerResponse::ProcessResult { backend, .. } => assert_eq!(
                backend,
                PluginHostBackend::NativeVst3,
                "process should use native backend for {}",
                plugin_path
            ),
            other => panic!("unexpected process response for {}: {:?}", plugin_path, other),
        }

        let (out_ch, _out_sr) = neowaves::audio_io::decode_audio_multi(&output).expect("decode output");
        assert_eq!(out_ch.len(), in_ch.len(), "channel mismatch for {}", plugin_path);
        assert_eq!(
            out_ch[0].len(),
            in_ch[0].len(),
            "length mismatch for {}",
            plugin_path
        );
        assert!(all_finite(&out_ch), "non-finite samples from {}", plugin_path);
        let diff_l = mean_abs_diff(&in_ch[0], &out_ch[0]);
        let diff_r = mean_abs_diff(&in_ch[1], &out_ch[1]);
        assert!(
            diff_l > 0.000_001 || diff_r > 0.000_001,
            "waveform unchanged for {} (diff_l={}, diff_r={})",
            plugin_path,
            diff_l,
            diff_r
        );
    }

    assert!(
        checked > 0,
        "no target VST3 plugins found under Program Files/Common Files/VST3"
    );
    let _ = std::fs::remove_dir_all(dir);
}
