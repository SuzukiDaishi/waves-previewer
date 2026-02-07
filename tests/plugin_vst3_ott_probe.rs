#![cfg(windows)]

use std::path::Path;

use neowaves::plugin::{PluginHostBackend, WorkerRequest, WorkerResponse};

const OTT_VST3_PATH: &str = r"C:\Program Files\Common Files\VST3\OTT.vst3";

#[test]
fn ott_vst3_probe_returns_native_params_when_installed() {
    if !Path::new(OTT_VST3_PATH).exists() {
        eprintln!("skip: OTT not installed at {}", OTT_VST3_PATH);
        return;
    }

    let resp = neowaves::plugin::client::run_request(&WorkerRequest::Probe {
        plugin_path: OTT_VST3_PATH.to_string(),
    })
    .expect("worker probe should return a response");

    match resp {
        WorkerResponse::ProbeResult {
            backend, params, ..
        } => {
            assert_eq!(
                backend,
                PluginHostBackend::NativeVst3,
                "OTT probe should stay on native VST3 backend"
            );
            assert!(
                !params.is_empty(),
                "OTT probe should expose native parameters"
            );
        }
        other => panic!("unexpected response: {other:?}"),
    }
}

