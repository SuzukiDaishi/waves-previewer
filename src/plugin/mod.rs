mod backends;
/// Block size the app asks a plugin to be prepared for when processing a
/// file offline: about 21 ms at 48 kHz, large enough to keep call overhead
/// low, small enough that no plugin refuses it.
pub const OFFLINE_MAX_BLOCK_SIZE: usize = 1024;
/// Largest block ever handed to a plugin backend, whatever the host asked
/// for; several plugins allocate per-block scratch sized to this.
pub const BACKEND_MAX_BLOCK_SIZE: usize = 4096;

pub mod catalog;
pub mod client;
pub mod gui_worker;
pub mod protocol;
pub mod worker;

pub use protocol::{
    GuiCapabilities, GuiSessionStatus, PluginChainSlotConfig, PluginDescriptorInfo, PluginFormat,
    PluginHostBackend, PluginParamInfo, PluginParamValue, WorkerRequest, WorkerResponse,
};
