pub mod bridge;
pub mod prompts;
pub mod resources;
pub mod server;
pub mod state;
pub mod tools;
pub mod types;

pub use bridge::{UiBridge, UiCommand, UiCommandResult};
pub use server::McpServer;
pub use state::McpState;

pub const DEFAULT_HTTP_ADDR: &str = "127.0.0.1:7464";
