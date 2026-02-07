use neowaves::plugin::{PluginHostBackend, PluginParamValue, WorkerRequest, WorkerResponse};

#[test]
fn protocol_processfx_roundtrip_keeps_state_blob_and_backend() {
    let req = WorkerRequest::ProcessFx {
        plugin_path: "C:/Plugins/Demo.vst3".to_string(),
        input_audio_path: "C:/tmp/in.wav".to_string(),
        output_audio_path: "C:/tmp/out.wav".to_string(),
        sample_rate: 48_000,
        max_block_size: 1024,
        enabled: true,
        bypass: false,
        state_blob_b64: Some("AQIDBA".to_string()),
        params: vec![PluginParamValue {
            id: "p0".to_string(),
            normalized: 0.33,
        }],
    };
    let raw = serde_json::to_vec(&req).expect("serialize");
    let restored: WorkerRequest = serde_json::from_slice(&raw).expect("deserialize");
    match restored {
        WorkerRequest::ProcessFx {
            state_blob_b64,
            sample_rate,
            max_block_size,
            ..
        } => {
            assert_eq!(sample_rate, 48_000);
            assert_eq!(max_block_size, 1024);
            assert_eq!(state_blob_b64.as_deref(), Some("AQIDBA"));
        }
        _ => panic!("unexpected request type"),
    }

    let resp = WorkerResponse::ProcessResult {
        output_audio_path: "C:/tmp/out.wav".to_string(),
        state_blob_b64: Some("AQIDBA".to_string()),
        backend: PluginHostBackend::NativeVst3,
        backend_note: None,
    };
    let raw = serde_json::to_vec(&resp).expect("serialize");
    let restored: WorkerResponse = serde_json::from_slice(&raw).expect("deserialize");
    match restored {
        WorkerResponse::ProcessResult {
            state_blob_b64,
            backend,
            ..
        } => {
            assert_eq!(state_blob_b64.as_deref(), Some("AQIDBA"));
            assert_eq!(backend, PluginHostBackend::NativeVst3);
        }
        _ => panic!("unexpected response type"),
    }
}

#[test]
fn protocol_backend_roundtrip_includes_native_clap() {
    let resp = WorkerResponse::ProbeResult {
        plugin: neowaves::plugin::PluginDescriptorInfo {
            key: "demo".to_string(),
            name: "demo".to_string(),
            path: "demo.clap".to_string(),
            format: neowaves::plugin::PluginFormat::Clap,
        },
        params: Vec::new(),
        state_blob_b64: None,
        backend: PluginHostBackend::NativeClap,
        backend_note: None,
    };
    let raw = serde_json::to_vec(&resp).expect("serialize");
    let restored: WorkerResponse = serde_json::from_slice(&raw).expect("deserialize");
    match restored {
        WorkerResponse::ProbeResult { backend, .. } => {
            assert_eq!(backend, PluginHostBackend::NativeClap);
        }
        _ => panic!("unexpected response type"),
    }
}
