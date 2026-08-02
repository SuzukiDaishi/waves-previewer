use std::path::{Path, PathBuf};

use neowaves::plugin::{WorkerRequest, WorkerResponse};

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
fn worker_probe_vst3_reports_native_failure_without_generic_success() {
    let dir = unique_temp_dir("plugin_probe");
    let vst = dir.join("DemoProbe.vst3");
    std::fs::write(&vst, b"").expect("write vst3 placeholder");
    let req = WorkerRequest::Probe {
        plugin_path: vst.to_string_lossy().to_string(),
    };
    let resp = neowaves::plugin::worker::handle_request(req);
    assert!(matches!(
        resp,
        WorkerResponse::Error { ref message }
            if message.contains("native VST3 probe failed")
    ));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn worker_process_fx_rejects_invalid_native_plugin() {
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
        params: Vec::new(),
    };
    let resp = neowaves::plugin::worker::handle_request(req);
    assert!(matches!(
        resp,
        WorkerResponse::Error { ref message }
            if message.contains("native VST3 process failed")
    ));
    assert!(!output.is_file());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn worker_chain_bypass_copies_dry_audio_without_loading_slots() {
    let dir = unique_temp_dir("plugin_chain_bypass");
    let input = dir.join("in.wav");
    let output = dir.join("out.wav");
    write_test_wav(&input);
    let response = neowaves::plugin::worker::handle_request(WorkerRequest::ProcessChain {
        slots: vec![neowaves::plugin::PluginChainSlotConfig {
            slot_id: 1,
            plugin_path: dir.join("missing.vst3").to_string_lossy().to_string(),
            enabled: true,
            bypass: false,
            state_blob_b64: None,
            params: Vec::new(),
        }],
        input_audio_path: input.to_string_lossy().to_string(),
        output_audio_path: output.to_string_lossy().to_string(),
        sample_rate: 48_000,
        max_block_size: 512,
        chain_bypass: true,
    });
    assert!(matches!(
        response,
        WorkerResponse::ChainProcessResult {
            latency_samples: 0,
            underruns: 0,
            failed_slot: None,
            ..
        }
    ));
    assert_eq!(
        std::fs::read(input).unwrap(),
        std::fs::read(output).unwrap()
    );
    let _ = std::fs::remove_dir_all(dir);
}
