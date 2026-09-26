//! What the user asked for.

use std::path::Path;

use crate::task::Task;

/// One request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op<'a> {
    /// Install dependencies, running these tool-manager operations.
    Install {
        /// Operations for providers that take one, such as mise.
        operations: &'a [String],
    },
    /// Run a declared task.
    Run {
        /// The task.
        task: &'a Task,
        /// Arguments forwarded to it.
        args: &'a [String],
    },
    /// Invoke a task runner without selecting a named task.
    RunDefault {
        /// Arguments forwarded to the runner.
        args: &'a [String],
    },
    /// Execute a name through the provider's exec primitive.
    Exec {
        /// The name.
        name: &'a str,
        /// Arguments forwarded to it.
        args: &'a [String],
    },
    /// Execute a binary belonging to an explicitly selected package.
    ExecPackage {
        /// Package specifier.
        package: &'a str,
        /// Binary name.
        bin: &'a str,
        /// Arguments forwarded to the binary.
        args: &'a [String],
    },
    /// Run a source file.
    RunFile {
        /// The file.
        file: &'a Path,
        /// Arguments forwarded to it.
        args: &'a [String],
    },
    /// Run the ecosystem's test runner.
    Test {
        /// Arguments forwarded to it.
        args: &'a [String],
    },
    /// Remove install directories.
    Clean,
    /// Ask the provider about its own state.
    Health {
        /// Index in the provider's declared health checks.
        check: usize,
    },
}

impl Op<'_> {
    /// The op's name in refusals and reports.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Install { .. } => "install",
            Self::Run { .. } => "run",
            Self::RunDefault { .. } => "run-default",
            Self::Exec { .. } => "exec",
            Self::ExecPackage { .. } => "exec-package",
            Self::RunFile { .. } => "run-file",
            Self::Test { .. } => "test",
            Self::Clean => "clean",
            Self::Health { .. } => "health",
        }
    }
}
