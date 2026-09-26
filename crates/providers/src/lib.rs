//! Provider declarations and the registry.

pub mod bundler;
pub mod cargo;
pub mod composer;
pub mod deno;
pub mod go;
pub mod managers;
pub mod node;
pub mod powershell;
pub mod python;
pub mod registry;
pub mod runners;
pub mod workspace;

pub use registry::{PROVIDERS, REGISTRY};

mod version;

pub mod extract;
