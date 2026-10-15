//! wrapper-agent: runs one wrapper-lite process per Apple Music account and
//! exposes them behind a single wrapper-lite compatible HTTP endpoint.
//!
//! The agent runs wherever lite can run: inside the QEMU guest (lite started
//! directly) or natively on Linux (lite started in a user-namespace chroot via
//! [`sandbox`]).

pub mod accounts;
pub mod agent;
pub mod api;
pub mod apple;
pub mod backup;
pub mod events;
pub mod health;
pub mod lite;
pub mod login;
pub mod public;
pub mod router;
#[cfg(target_os = "linux")]
pub mod sandbox;
pub mod server;
pub mod settings;
pub mod stats;
#[cfg(test)]
mod tests;

pub use accounts::Layout;
pub use agent::{Agent, AgentConfig};
pub use events::{BusLayer, EventBus};
pub use lite::{LiteOptions, LiteRuntime};
