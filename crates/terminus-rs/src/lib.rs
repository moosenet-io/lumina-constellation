//! terminus-rs: Rust fallback tool implementations for the Chord proxy.

pub mod ansible;
pub mod approval;
pub mod axon;
pub mod commute;
pub mod dgem;
pub mod dura;
pub mod error;
pub mod gitea;
pub mod dev;
pub mod gateway;
pub mod github;
pub mod infisical;
pub mod network;
pub mod openhands;
pub mod google;
pub mod jellyseerr;
pub mod litellm;
pub mod portainer;
pub mod prometheus;
pub mod hearth;
pub mod ledger;
pub mod myelin;
pub mod news;
pub mod nexus;
pub mod plane;
pub mod registry;
pub mod relay;
pub mod seer;
pub mod tool;
pub mod vector;
pub mod vitals;
pub mod weather;
pub mod wizard;

pub use error::ToolError;
pub use registry::{register_all, ToolInfo, ToolRegistry};
pub use tool::RustTool;
