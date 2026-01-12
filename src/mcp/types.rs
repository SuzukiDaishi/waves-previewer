use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDescriptor {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListToolsResult {
    pub tools: Vec<ToolDescriptor>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListFilesArgs {
    pub query: Option<String>,
    pub regex: Option<bool>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
    pub include_meta: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileItem {
    pub path: String,
    pub name: String,
    pub folder: String,
    pub length_secs: Option<f32>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u16>,
    pub bits: Option<u16>,
    pub peak_db: Option<f32>,
    pub lufs_i: Option<f32>,
    pub gain_db: Option<f32>,
    pub status: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListFilesResult {
    pub total: u32,
    pub items: Vec<FileItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelectionArgs {
    pub paths: Vec<String>,
    pub open_tab: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelectionResult {
    pub selected_paths: Vec<String>,
    pub active_tab_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModeArgs {
    pub mode: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpeedArgs {
    pub rate: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PitchArgs {
    pub semitones: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StretchArgs {
    pub rate: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolumeArgs {
    pub db: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GainArgs {
    pub path: String,
    pub db: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GainClearArgs {
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoopArgs {
    pub path: String,
    pub start_samples: u64,
    pub end_samples: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WriteLoopArgs {
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportArgs {
    pub mode: String,
    pub dest_folder: Option<String>,
    pub name_template: Option<String>,
    pub conflict: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportResult {
    pub ok: u32,
    pub failed: u32,
    pub success_paths: Vec<String>,
    pub failed_paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenFolderArgs {
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenFilesArgs {
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScreenshotArgs {
    pub path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScreenshotResult {
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DebugSummary {
    pub selected_paths: Vec<String>,
    pub active_tab_path: Option<String>,
    pub mode: Option<String>,
    pub playing: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceDescriptor {
    pub uri: String,
    pub name: String,
    pub mime_type: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceContent {
    pub uri: String,
    pub mime_type: String,
    pub data: Option<String>,
    pub text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptDescriptor {
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptResult {
    pub content: String,
}
