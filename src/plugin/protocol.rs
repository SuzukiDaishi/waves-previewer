use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PluginFormat {
    Vst3,
    Clap,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PluginDescriptorInfo {
    pub key: String,
    pub name: String,
    pub path: String,
    pub format: PluginFormat,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PluginParamInfo {
    pub id: String,
    pub name: String,
    pub normalized: f32,
    pub default_normalized: f32,
    pub min: f32,
    pub max: f32,
    pub unit: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PluginParamValue {
    pub id: String,
    pub normalized: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PluginHostBackend {
    Generic,
    NativeVst3,
    NativeClap,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum WorkerRequest {
    Ping,
    Scan {
        search_paths: Vec<String>,
    },
    Probe {
        plugin_path: String,
    },
    ProcessFx {
        plugin_path: String,
        input_audio_path: String,
        output_audio_path: String,
        sample_rate: u32,
        max_block_size: usize,
        enabled: bool,
        bypass: bool,
        state_blob_b64: Option<String>,
        params: Vec<PluginParamValue>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum WorkerResponse {
    Pong,
    ScanResult {
        plugins: Vec<PluginDescriptorInfo>,
    },
    ProbeResult {
        plugin: PluginDescriptorInfo,
        params: Vec<PluginParamInfo>,
        state_blob_b64: Option<String>,
        backend: PluginHostBackend,
        #[serde(default)]
        backend_note: Option<String>,
    },
    ProcessResult {
        output_audio_path: String,
        state_blob_b64: Option<String>,
        backend: PluginHostBackend,
        #[serde(default)]
        backend_note: Option<String>,
    },
    Error {
        message: String,
    },
}
