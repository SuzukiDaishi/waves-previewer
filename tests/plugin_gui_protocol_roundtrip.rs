use neowaves::plugin::{
    GuiCapabilities, PluginHostBackend, PluginParamValue, WorkerRequest, WorkerResponse,
};

#[test]
fn gui_session_request_roundtrip() {
    let req = WorkerRequest::GuiSessionOpen {
        session_id: 42,
        plugin_path: "C:/Plugins/Demo.vst3".to_string(),
        state_blob_b64: Some("AQID".to_string()),
        params: vec![PluginParamValue {
            id: "vst3:00000001".to_string(),
            normalized: 0.25,
        }],
    };
    let raw = serde_json::to_vec(&req).expect("serialize");
    let restored: WorkerRequest = serde_json::from_slice(&raw).expect("deserialize");
    match restored {
        WorkerRequest::GuiSessionOpen {
            session_id,
            plugin_path,
            state_blob_b64,
            params,
        } => {
            assert_eq!(session_id, 42);
            assert!(plugin_path.ends_with(".vst3"));
            assert_eq!(state_blob_b64.as_deref(), Some("AQID"));
            assert_eq!(params.len(), 1);
        }
        other => panic!("unexpected request: {other:?}"),
    }
}

#[test]
fn gui_opened_response_roundtrip() {
    let resp = WorkerResponse::GuiOpened {
        session_id: 7,
        backend: PluginHostBackend::NativeVst3,
        params: Vec::new(),
        state_blob_b64: None,
        capabilities: GuiCapabilities {
            supports_native_gui: true,
            supports_param_feedback: true,
            supports_state_sync: false,
        },
        backend_note: Some("note".to_string()),
    };
    let raw = serde_json::to_vec(&resp).expect("serialize");
    let restored: WorkerResponse = serde_json::from_slice(&raw).expect("deserialize");
    match restored {
        WorkerResponse::GuiOpened {
            session_id,
            backend,
            capabilities,
            backend_note,
            ..
        } => {
            assert_eq!(session_id, 7);
            assert_eq!(backend, PluginHostBackend::NativeVst3);
            assert!(capabilities.supports_native_gui);
            assert_eq!(backend_note.as_deref(), Some("note"));
        }
        other => panic!("unexpected response: {other:?}"),
    }
}
