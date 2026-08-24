pub mod app;
pub mod audio;
pub mod audio_asset;
pub mod audio_capture;
pub mod audio_channels;
pub mod audio_io;
pub mod cli;
pub mod crash_report;
pub mod flac_meta;
pub mod ipc;
#[cfg(feature = "mp3_lame")]
pub mod lame;
pub mod loop_markers;
pub mod markers;
pub mod media_kind;
pub mod metadata;
pub mod meter;
pub mod plugin;
pub mod ui_wake;
pub mod video;
pub mod wav_stream;
pub mod wave;

pub use app::{
    FadeShape, LoopMode, LoopXfadeShape, PlaybackTimelineMap, PluginFxChainDraft, PluginFxSlot,
    PluginPreviewEngine, PluginProbeCapabilities, StartupConfig, TranscriptDocument,
    TranscriptFreshness, ViewMode, WavesPreviewer,
};
pub use audio_asset::{
    AssetAccess, AssetRevision, AudioAssetDescriptor, AudioAssetId, AudioBacking,
};

#[cfg(feature = "kittest")]
pub mod kittest;
