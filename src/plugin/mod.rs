mod backends;
pub mod catalog;
pub mod client;
pub mod gui_worker;
pub mod protocol;
pub mod worker;

pub use protocol::{
    GuiCapabilities, GuiSessionStatus, PluginChainSlotConfig, PluginDescriptorInfo, PluginFormat,
    PluginHostBackend, PluginParamInfo, PluginParamValue, WorkerRequest, WorkerResponse,
};
