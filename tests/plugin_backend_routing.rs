use std::path::{Path, PathBuf};

use neowaves::plugin::{PluginHostBackend, WorkerRequest, WorkerResponse};

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
    let len = 4096usize;
    let mut ch = vec![0.0f32; len];
    for (i, v) in ch.iter_mut().enumerate() {
        let t = i as f32 / sr as f32;
        *v = (2.0 * std::f32::consts::PI * 440.0 * t).sin() * 0.2;
    }
    neowaves::wave::export_channels_audio(&[ch], sr, path).expect("write test wav");
}

#[test]
fn probe_backend_routes_by_extension() {
    let dir = unique_temp_dir("plugin_route_probe");
    let vst = dir.join("DemoRoute.vst3");
    let clap = dir.join("DemoRoute.clap");
    std::fs::write(&vst, b"").expect("write vst3 placeholder");
    std::fs::write(&clap, b"").expect("write clap placeholder");

    let vst_resp = neowaves::plugin::worker::handle_request(WorkerRequest::Probe {
        plugin_path: vst.to_string_lossy().to_string(),
    });
    match vst_resp {
        WorkerResponse::ProbeResult { backend, .. } => {
            assert!(matches!(
                backend,
                PluginHostBackend::Generic | PluginHostBackend::NativeVst3
            ));
        }
        other => panic!("unexpected response: {other:?}"),
    }

    let clap_resp = neowaves::plugin::worker::handle_request(WorkerRequest::Probe {
        plugin_path: clap.to_string_lossy().to_string(),
    });
    match clap_resp {
        WorkerResponse::ProbeResult { backend, .. } => {
            assert!(matches!(
                backend,
                PluginHostBackend::Generic | PluginHostBackend::NativeClap
            ));
        }
        other => panic!("unexpected response: {other:?}"),
    }

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn process_backend_fallback_stays_deterministic() {
    let dir = unique_temp_dir("plugin_route_process");
    let input = dir.join("in.wav");
    let output = dir.join("out.wav");
    let plugin = dir.join("DemoRoute.clap");
    write_test_wav(&input);
    std::fs::write(&plugin, b"").expect("write clap placeholder");

    let resp = neowaves::plugin::worker::handle_request(WorkerRequest::ProcessFx {
        plugin_path: plugin.to_string_lossy().to_string(),
        input_audio_path: input.to_string_lossy().to_string(),
        output_audio_path: output.to_string_lossy().to_string(),
        sample_rate: 48_000,
        max_block_size: 512,
        enabled: true,
        bypass: false,
        state_blob_b64: None,
        params: Vec::new(),
    });
    match resp {
        WorkerResponse::ProcessResult {
            output_audio_path,
            backend,
            ..
        } => {
            assert!(PathBuf::from(output_audio_path).is_file());
            assert!(matches!(
                backend,
                PluginHostBackend::Generic | PluginHostBackend::NativeClap
            ));
        }
        other => panic!("unexpected response: {other:?}"),
    }

    let _ = std::fs::remove_dir_all(dir);
}
