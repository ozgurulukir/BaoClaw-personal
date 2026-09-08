//! Sandbox support: the live `--sandbox bwrap|docker` flag path.
//!
//! The command-line builders for both backends live in the split inherent
//! impl on [`infra::sandbox_config::SandboxConfig`], included here via
//! `#[path]` and re-exported for `startup`.

#[path = "../sandbox_legacy.rs"]
mod legacy;

pub use legacy::{SandboxBackend, SandboxConfig};
