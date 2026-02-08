use std::path::{Path, PathBuf};

use neowaves::plugin::{PluginHostBackend, PluginParamValue, WorkerRequest, WorkerResponse};

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
    let len = 4800usize;
    let mut ch = vec![0.0f32; len];
    for (i, v) in ch.iter_mut().enumerate() {
        let t = i as f32 / sr as f32;
        *v = (2.0 * std::f32::consts::PI * 220.0 * t).sin() * 0.2;
    }
    neowaves::wave::export_channels_audio(&[ch], sr, path).expect("write test wav");
}

#[test]
fn worker_scan_detects_vst3_and_clap() {
    let dir = unique_temp_dir("plugin_scan");
    let vst = dir.join("DemoA.vst3");
    let clap = dir.join("DemoB.clap");
    std::fs::write(&vst, b"").expect("write vst3 placeholder");
    std::fs::write(&clap, b"").expect("write clap placeholder");
    let req = WorkerRequest::Scan {
        search_paths: vec![dir.to_string_lossy().to_string()],
    };
    let resp = neowaves::plugin::worker::handle_request(req);
    match resp {
        WorkerResponse::ScanResult { plugins } => {
            let keys: Vec<String> = plugins.iter().map(|p| p.key.clone()).collect();
            assert!(keys.iter().any(|k| k.ends_with(".vst3")));
            assert!(keys.iter().any(|k| k.ends_with(".clap")));
        }
        other => panic!("unexpected response: {other:?}"),
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn worker_probe_rejects_unknown_extension() {
    let req = WorkerRequest::Probe {
        plugin_path: "C:/tmp/not_plugin.txt".to_string(),
    };
    let resp = neowaves::plugin::worker::handle_request(req);
    match resp {
        WorkerResponse::Error { message } => {
            assert!(message.contains("unsupported plugin format"));
        }
        other => panic!("unexpected response: {other:?}"),
    }
}

#[test]
fn worker_probe_vst3_uses_generic_backend_when_native_unavailable() {
    let dir = unique_temp_dir("plugin_probe");
    let vst = dir.join("DemoProbe.vst3");
    std::fs::write(&vst, b"").expect("write vst3 placeholder");
    let req = WorkerRequest::Probe {
        plugin_path: vst.to_string_lossy().to_string(),
    };
    let resp = neowaves::plugin::worker::handle_request(req);
    match resp {
        WorkerResponse::ProbeResult {
            plugin,
            params,
            state_blob_b64,
            backend,
            capabilities: _,
            backend_note,
        } => {
            assert_eq!(plugin.path, vst.to_string_lossy());
            assert_eq!(backend, PluginHostBackend::Generic);
            assert!(state_blob_b64.is_none());
            assert!(params.is_empty());
            assert!(backend_note.is_some());
        }
        other => panic!("unexpected response: {other:?}"),
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn worker_process_fx_writes_output_audio() {
    let dir = unique_temp_dir("plugin_process");
    let input = dir.join("in.wav");
    let output = dir.join("out.wav");
    write_test_wav(&input);
    let req = WorkerRequest::ProcessFx {
        plugin_path: "dummy.vst3".to_string(),
        input_audio_path: input.to_string_lossy().to_string(),
        output_audio_path: output.to_string_lossy().to_string(),
        sample_rate: 48_000,
        max_block_size: 512,
        enabled: true,
        bypass: false,
        state_blob_b64: None,
        params: vec![
            PluginParamValue {
                id: "mix".to_string(),
                normalized: 1.0,
            },
            PluginParamValue {
                id: "output_gain_db".to_string(),
                normalized: 0.5,
            },
        ],
    };
    let resp = neowaves::plugin::worker::handle_request(req);
    match resp {
        WorkerResponse::ProcessResult {
            output_audio_path,
            state_blob_b64,
            backend,
            backend_note,
        } => {
            assert_eq!(PathBuf::from(output_audio_path), output);
            assert!(state_blob_b64.is_some());
            assert_eq!(backend, PluginHostBackend::Generic);
            assert!(backend_note.is_some());
            let (ch, sr) = neowaves::audio_io::decode_audio_multi(&output).expect("decode output");
            assert_eq!(sr, 48_000);
            assert_eq!(ch.len(), 1);
            assert_eq!(ch[0].len(), 4800);
        }
        other => panic!("unexpected response: {other:?}"),
    }
    let _ = std::fs::remove_dir_all(dir);
}
