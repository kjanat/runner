//! In-process command execution via the cross-platform shell from
//! [`deno_task_shell`].
//!
//! A reusable engine for running a shell command string without a system
//! shell or any specific tool binary: sequential lists, `&&`/`||`, pipes,
//! env-var expansion, redirects, globs, and a set of coreutils-style
//! builtins. It is a *subset* of POSIX `sh` (not bash), and external
//! command words still resolve from `$PATH`.
//!
//! Used today by deno self-exec ([`super::deno_exec`]); any source whose
//! task bodies are shell strings (e.g. `package.json` scripts) can build
//! on it, provided that source's own semantics (env injection, local
//! `bin` dirs, lifecycle hooks) are layered on top.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::Path;

use anyhow::{Context, Result, anyhow};
use deno_task_shell::{KillSignal, execute, parser};

/// Run `command` (with `args` appended) in `cwd`, returning the exit code.
///
/// Errors only on a parse or runtime-construction failure; a non-zero
/// command exit is returned as the code, not an error. Inherits the
/// current process environment.
pub(crate) fn run(command: &str, args: &[String], cwd: &Path) -> Result<i32> {
    let mut script = command.to_string();
    for arg in args {
        let quoted = shlex::try_quote(arg).map_err(|e| anyhow!("cannot quote arg: {e}"))?;
        script.push(' ');
        script.push_str(&quoted);
    }

    let list = parser::parse(&script).map_err(|e| anyhow!("failed to parse command: {e}"))?;
    let env: HashMap<OsString, OsString> = std::env::vars_os().collect();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("failed to build async runtime for in-process shell")?;

    Ok(runtime.block_on(execute(
        list,
        env,
        cwd.to_path_buf(),
        HashMap::new(),
        KillSignal::default(),
    )))
}

/// Conservative check for whether `command` invokes `program` as a
/// command word: any shell token equal to `program`. Over-detection
/// (e.g. `program` appearing as an argument) is the safe direction for
/// callers gating on "needs this binary".
pub(crate) fn mentions_program(command: &str, program: &str) -> bool {
    shlex::split(command)
        .unwrap_or_default()
        .iter()
        .any(|token| token == program)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::test_support::TempDir;

    #[test]
    fn run_executes_builtin_command() {
        let dir = TempDir::new("shell-run-ok");
        let code = run("exit 0", &[], dir.path()).expect("builtin should run");
        assert_eq!(code, 0);
    }

    #[test]
    fn run_propagates_nonzero_exit() {
        let dir = TempDir::new("shell-run-fail");
        let code = run("exit 3", &[], dir.path()).expect("builtin should run");
        assert_eq!(code, 3);
    }

    #[test]
    fn run_appends_quoted_args() {
        // `false <args>` ignores args but must still parse with them
        // appended and quoted; a space-bearing arg must not split.
        let dir = TempDir::new("shell-run-args");
        let code = run("exit", &["7".to_string()], dir.path()).expect("should run");
        assert_eq!(code, 7);
    }

    #[test]
    fn mentions_program_detects_command_word() {
        assert!(mentions_program("deno run -A x.ts", "deno"));
        assert!(mentions_program("tsc && deno test", "deno"));
        assert!(!mentions_program("echo hello", "deno"));
    }
}
