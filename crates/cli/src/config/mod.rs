//! `runner.toml` and the setting values it shares with flags and variables.

mod key;
mod load;
mod values;

pub(crate) use key::KeyPath;
#[cfg(feature = "lsp")]
pub(crate) use key::toml_key;
pub(crate) use load::*;
pub(crate) use values::boolean;
