//! Deno-specific layer over the in-process shell ([`super::shell`]).
//!
//! Classifies `deno.json` tasks for self-execution and runs the eligible
//! ones without the `deno` binary. The shell grammar/execution itself
//! lives in [`super::shell`]; this module only adds deno's task model
//! (object vs string form, `cwd`, `dependencies`, `deno` invocation).

use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;
use serde::Deserialize;

/// A deno task classified for self-execution.
#[derive(Debug)]
pub(crate) struct DenoTaskPlan {
    /// The task's command string (object `command` or bare-string form).
    command: String,
    /// Optional per-task working directory (object `cwd`), resolved by
    /// [`run`] relative to the base cwd.
    cwd: Option<String>,
    /// Whether the task declares `dependencies` (unsupported in v1).
    has_dependencies: bool,
    /// Whether the command invokes `deno`, which needs the binary even
    /// through the embedded shell.
    invokes_deno: bool,
}

impl DenoTaskPlan {
    /// Self-executable in v1: a leaf command that neither declares
    /// `dependencies` nor invokes `deno`.
    pub(crate) const fn self_executable(&self) -> bool {
        !self.has_dependencies && !self.invokes_deno
    }
}

/// Classify `task` from the deno config at `config_path`.
///
/// Returns `None` when the config can't be read/parsed, the task is
/// absent, or it has no command body (a pure-`dependencies` task).
pub(crate) fn plan(config_path: &Path, task: &str) -> Option<DenoTaskPlan> {
    #[derive(Deserialize)]
    struct Partial {
        tasks: Option<HashMap<String, serde_json::Value>>,
    }
    let content = std::fs::read_to_string(config_path).ok()?;
    let parsed = json5::from_str::<Partial>(&content).ok()?;
    let value = parsed.tasks?.remove(task)?;

    let (command, cwd, has_dependencies) = match value {
        serde_json::Value::String(command) => (command, None, false),
        serde_json::Value::Object(map) => {
            let command = map
                .get("command")
                .and_then(serde_json::Value::as_str)?
                .to_string();
            let cwd = map
                .get("cwd")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);
            let has_dependencies = map
                .get("dependencies")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|deps| !deps.is_empty());
            (command, cwd, has_dependencies)
        }
        _ => return None,
    };

    Some(DenoTaskPlan {
        invokes_deno: super::shell::mentions_program(&command, "deno"),
        command,
        cwd,
        has_dependencies,
    })
}

/// Run a self-executable deno task in-process, returning its exit code.
///
/// `args` are appended to the command, matching `deno task <name>
/// <args...>`. The per-task `cwd` resolves relative to `cwd` (the
/// invocation root).
pub(crate) fn run(
    plan: &DenoTaskPlan,
    args: &[String],
    cwd: &Path,
    stdout: crate::tool::TaskStream,
    stderr: crate::tool::TaskStream,
) -> Result<i32> {
    let effective_cwd = plan
        .cwd
        .as_ref()
        .map_or_else(|| cwd.to_path_buf(), |rel| cwd.join(rel));
    super::shell::run(&plan.command, args, &effective_cwd, stdout, stderr)
}
