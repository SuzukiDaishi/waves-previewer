#![cfg(windows)]

use std::path::Path;
use std::path::PathBuf;

use neowaves::plugin::{PluginHostBackend, PluginParamValue, WorkerRequest, WorkerResponse};

const OTT_VST3_PATH: &str = r"C:\Program Files\Common Files\VST3\OTT.vst3";

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
    let len = sr as usize * 2;
    let mut l = vec![0.0f32; len];
    let mut r = vec![0.0f32; len];
    for i in 0..len {
        let t = i as f32 / sr as f32;
        l[i] = (2.0 * std::f32::consts::PI * 220.0 * t).sin() * 0.22;
        r[i] = (2.0 * std::f32::consts::PI * 440.0 * t).sin() * 0.18;
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

fn pick_test_params(params: &[neowaves::plugin::PluginParamInfo]) -> Vec<PluginParamValue> {
    let mut out: Vec<PluginParamValue> = params
        .iter()
        .map(|p| PluginParamValue {
            id: p.id.clone(),
            normalized: p.default_normalized.clamp(0.0, 1.0),
        })
        .collect();

    for p in &mut out {
        if params
            .iter()
            .find(|x| x.id == p.id)
            .map(|x| x.name.to_ascii_lowercase().contains("bypass"))
            .unwrap_or(false)
        {
            p.normalized = 0.0;
        }
    }

    if let Some((idx, _)) = params
        .iter()
        .enumerate()
        .find(|(_, p)| p.name.to_ascii_lowercase().contains("depth"))
    {
        out[idx].normalized = 1.0;
    } else if let Some((idx, _)) = params.iter().enumerate().find(|(_, p)| {
        let name = p.name.to_ascii_lowercase();
        name.contains("mix")
            || name.contains("amount")
            || name.contains("strength")
            || name.contains("gain")
    }) {
        out[idx].normalized = 1.0;
    }

    out
}

#[test]
fn ott_vst3_process_runs_on_native_backend() {
    if !Path::new(OTT_VST3_PATH).exists() {
        eprintln!("skip: OTT not installed at {}", OTT_VST3_PATH);
        return;
    }

    let probe = neowaves::plugin::client::run_request(&WorkerRequest::Probe {
        plugin_path: OTT_VST3_PATH.to_string(),
    })
    .expect("probe should return response");

    let params = match probe {
        WorkerResponse::ProbeResult {
            backend,
            params,
            ..
        } => {
            assert_eq!(backend, PluginHostBackend::NativeVst3);
            assert!(!params.is_empty(), "OTT params should not be empty");
            params
        }
        other => panic!("unexpected probe response: {other:?}"),
    };

    let dir = unique_temp_dir("plugin_vst3_native_process");
    let input = dir.join("in.wav");
    let output = dir.join("out.wav");
    write_stereo_test_wav(&input);

    let req = WorkerRequest::ProcessFx {
        plugin_path: OTT_VST3_PATH.to_string(),
        input_audio_path: input.to_string_lossy().to_string(),
        output_audio_path: output.to_string_lossy().to_string(),
        sample_rate: 48_000,
        max_block_size: 1024,
        enabled: true,
        bypass: false,
        state_blob_b64: None,
        params: pick_test_params(&params),
    };
    let resp = neowaves::plugin::client::run_request(&req).expect("process should return response");

    match resp {
        WorkerResponse::ProcessResult { backend, .. } => {
            assert_eq!(
                backend,
                PluginHostBackend::NativeVst3,
                "OTT process should stay on native backend"
            );
        }
        other => panic!("unexpected process response: {other:?}"),
    }

    let (in_ch, _in_sr) = neowaves::audio_io::decode_audio_multi(&input).expect("decode input");
    let (out_ch, _out_sr) = neowaves::audio_io::decode_audio_multi(&output).expect("decode output");
    assert_eq!(in_ch.len(), out_ch.len());
    assert_eq!(in_ch[0].len(), out_ch[0].len());
    let diff_l = mean_abs_diff(&in_ch[0], &out_ch[0]);
    let diff_r = mean_abs_diff(&in_ch[1], &out_ch[1]);
    assert!(
        diff_l > 0.000_01 || diff_r > 0.000_01,
        "native process should change OTT waveform; diff_l={diff_l}, diff_r={diff_r}"
    );

    let _ = std::fs::remove_dir_all(dir);
}
