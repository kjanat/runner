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
use deno_task_shell::{
    KillSignal, ShellPipeReader, ShellPipeWriter, ShellState, execute_with_pipes, parser,
};

/// Run `command` (with `args` appended) in `cwd`, returning the exit code.
///
/// Errors only on a parse or runtime-construction failure; a non-zero
/// command exit is returned as the code, not an error. Inherits the
/// current process environment.
pub(crate) fn run(
    command: &str,
    args: &[String],
    cwd: &Path,
    stdout: crate::tool::TaskStream,
    stderr: crate::tool::TaskStream,
) -> Result<i32> {
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

    let state = ShellState::new(
        env,
        cwd.to_path_buf(),
        HashMap::new(),
        KillSignal::default(),
    );
    let stdout = match stdout {
        crate::tool::TaskStream::Inherit => ShellPipeWriter::stdout(),
        crate::tool::TaskStream::Discard => ShellPipeWriter::null(),
    };
    let stderr = match stderr {
        crate::tool::TaskStream::Inherit => ShellPipeWriter::stderr(),
        crate::tool::TaskStream::Discard => ShellPipeWriter::null(),
    };
    Ok(runtime.block_on(execute_with_pipes(
        list,
        state,
        ShellPipeReader::stdin(),
        stdout,
        stderr,
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
