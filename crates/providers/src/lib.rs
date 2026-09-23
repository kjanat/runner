//! Provider declarations and the registry.

pub mod bundler;
pub mod cargo;
pub mod composer;
pub mod deno;
pub mod go;
pub mod managers;
pub mod node;
pub mod python;
pub mod registry;
pub mod runners;

pub use registry::{PROVIDERS, REGISTRY};
