//! Subcommand implementations: info, run, install, clean, list, exec, completions.

mod clean;
mod completions;
mod exec;
mod info;
mod install;
mod list;
mod run;

pub use clean::clean;
pub use completions::completions;
pub use exec::exec;
pub use info::info;
pub use install::install;
pub use list::list;
pub use run::run;
