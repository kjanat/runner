//! The settings of one invocation: the command line, `RUNNER_*` variables and
//! `runner.toml`, resolved once into [`ResolutionOverrides`].

mod error;
mod overrides;
pub(crate) mod probe;
mod types;

pub(crate) use error::ResolveError;
pub(crate) use overrides::{Invocation, config_issues, validate_config};
pub(crate) use probe::probe_in as probe_path_for_doctor;
#[cfg(test)]
pub(crate) use types::{
    DownloadPolicy, Output, PmOverride, RuntimeOverride, SourceOverride, TaskChoice,
};
pub(crate) use types::{LockfilePolicy, OverrideOrigin, ResolutionOverrides, ScriptPolicy};
