//! Core domain primitives for Synapse.

pub mod knowledge;

/// The Synapse workspace version exposed to local clients.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
