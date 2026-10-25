//! Jani's trusted shared kernel.
//! The kernel has no dependency on the model loop, plugins, server or coordinator.
//! The domain contracts are generated from repository-root contract/ by build.rs.

pub mod approvals;
pub mod budget;
pub mod config;
pub mod contract;
pub mod crypto;
pub mod data;
pub mod identity;
pub mod journal;
pub mod permissions;
pub mod policy;
pub mod process;
pub mod schema;
pub mod settings;
pub mod types;
