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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PluginChainSlotConfig {
    pub slot_id: u64,
    pub plugin_path: String,
    pub enabled: bool,
    pub bypass: bool,
    pub state_blob_b64: Option<String>,
    pub params: Vec<PluginParamValue>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PluginHostBackend {
    Generic,
    NativeVst3,
    NativeClap,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct GuiCapabilities {
    pub supports_native_gui: bool,
    pub supports_param_feedback: bool,
    pub supports_state_sync: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum GuiSessionStatus {
    #[default]
    Closed,
    Opening,
    Live,
    Error,
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
    ProcessChain {
        slots: Vec<PluginChainSlotConfig>,
        input_audio_path: String,
        output_audio_path: String,
        sample_rate: u32,
        max_block_size: usize,
        chain_bypass: bool,
    },
    ChainSessionOpen {
        session_id: u64,
        slots: Vec<PluginChainSlotConfig>,
        sample_rate: u32,
        channels: u16,
        max_block_size: usize,
        render_ahead_ms: u32,
    },
    ChainSessionConfigure {
        session_id: u64,
        slots: Vec<PluginChainSlotConfig>,
        chain_bypass: bool,
    },
    ChainSessionProcess {
        session_id: u64,
        input_audio_path: String,
        output_audio_path: String,
    },
    ChainSessionSeek {
        session_id: u64,
        source_frame: u64,
    },
    ChainSessionFlush {
        session_id: u64,
    },
    ChainSessionClose {
        session_id: u64,
    },
    GuiSessionOpen {
        session_id: u64,
        plugin_path: String,
        state_blob_b64: Option<String>,
        params: Vec<PluginParamValue>,
    },
    GuiSessionPoll {
        session_id: u64,
    },
    GuiSessionClose {
        session_id: u64,
    },
    /// GUI worker が生きているかを確認するための軽量 ping。
    /// GUI worker は即座に HeartbeatAck を返す（ブロッキング処理なし）。
    Heartbeat {
        request_id: u64,
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
        capabilities: GuiCapabilities,
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
    ChainProcessResult {
        output_audio_path: String,
        slot_state_blobs_b64: Vec<(u64, Option<String>)>,
        latency_samples: u32,
        underruns: u64,
        failed_slot: Option<u64>,
        backend_note: Option<String>,
    },
    ChainSessionOpened {
        session_id: u64,
        render_ahead_ms: u32,
    },
    ChainSessionAck {
        session_id: u64,
        latency_samples: u32,
        underruns: u64,
    },
    GuiOpened {
        session_id: u64,
        backend: PluginHostBackend,
        params: Vec<PluginParamInfo>,
        state_blob_b64: Option<String>,
        #[serde(default)]
        capabilities: GuiCapabilities,
        #[serde(default)]
        backend_note: Option<String>,
    },
    GuiSnapshot {
        session_id: u64,
        params: Vec<PluginParamValue>,
        state_blob_b64: Option<String>,
        backend: PluginHostBackend,
        closed: bool,
        #[serde(default)]
        backend_note: Option<String>,
    },
    GuiClosed {
        session_id: u64,
        state_blob_b64: Option<String>,
        backend: PluginHostBackend,
        #[serde(default)]
        backend_note: Option<String>,
    },
    GuiError {
        session_id: u64,
        message: String,
    },
    /// Heartbeat リクエストへの即時応答。
    HeartbeatAck {
        request_id: u64,
    },
    Error {
        message: String,
    },
}
