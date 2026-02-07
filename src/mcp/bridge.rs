use std::sync::mpsc::{Receiver, Sender};

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::mcp::types::{
    ExportArgs, GainArgs, GainClearArgs, ListFilesArgs, LoopArgs, ModeArgs, OpenFilesArgs,
    OpenFolderArgs, PitchArgs, ScreenshotArgs, SelectionArgs, SpeedArgs, StretchArgs, VolumeArgs,
    WriteLoopArgs,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum UiCommand {
    ListFiles(ListFilesArgs),
    GetSelection,
    SetSelection(SelectionArgs),
    Play,
    Stop,
    SetVolume(VolumeArgs),
    SetMode(ModeArgs),
    SetSpeed(SpeedArgs),
    SetPitch(PitchArgs),
    SetStretch(StretchArgs),
    ApplyGain(GainArgs),
    ClearGain(GainClearArgs),
    SetLoopMarkers(LoopArgs),
    WriteLoopMarkers(WriteLoopArgs),
    Export(ExportArgs),
    OpenFolder(OpenFolderArgs),
    OpenFiles(OpenFilesArgs),
    Screenshot(ScreenshotArgs),
    DebugSummary,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiCommandResult {
    pub ok: bool,
    pub payload: Value,
    pub error: Option<String>,
}

pub struct UiBridge {
    tx: Sender<UiCommand>,
    rx: Receiver<UiCommandResult>,
}

impl UiBridge {
    pub fn new(tx: Sender<UiCommand>, rx: Receiver<UiCommandResult>) -> Self {
        Self { tx, rx }
    }

    pub fn send(&self, cmd: UiCommand) -> Result<UiCommandResult> {
        self.tx
            .send(cmd)
            .map_err(|e| anyhow!("bridge send failed: {e}"))?;
        self.rx
            .recv()
            .map_err(|e| anyhow!("bridge recv failed: {e}"))
    }
}
