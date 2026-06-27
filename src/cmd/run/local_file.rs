//! Local-file execution for `run <path>`.
//!
//! A token that points at a local file should be *run as that file* — never
//! handed to a package manager's package-exec primitive (`bunx`/`npx`/
//! `pnpm dlx`/`deno x`/`uvx`), which would resolve a local path as a remote
//! package spec and fail with a registry 404 or a `git clone` error.
//!
//! [`try_build`] runs at the top of [`super::dispatch::resolve_dispatch`],
//! before task lookup and the PM-exec fallback. It classifies a path-like
//! token into one of four outcomes:
//!
//! 1. **Executable file** → spawned directly (`Command::new(path)`); on Unix
//!    the kernel honors any `#!` line itself.
//! 2. **Non-executable file with a `#!` shebang** → the interpreter is
//!    parsed (including `#!/usr/bin/env -S <interp> <args>`) and the file is
//!    run through it.
//! 3. **Recognized source extension** → run via the project runtime
//!    (`.ts`/`.js`/… → bun / `deno run` / node; `.py` → `uv run` / python;
//!    `.go` → `go run`), *not* package-exec.
//! 4. **Otherwise** → a clear, actionable error.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context as _, Result, bail};

use crate::resolver::ResolutionOverrides;
use crate::tool;
use crate::types::{Ecosystem, PackageManager, ProjectContext};

/// A resolved local-file dispatch: the spawnable command plus the short
/// label shown in the `→` dispatch-arrow trace.
#[derive(Debug)]
pub(super) struct LocalDispatch {
    pub(super) label: String,
    pub(super) command: Command,
}

/// Try to interpret an explicit path-like `token` (one carrying a separator
/// or a `~`/`./`/`/` prefix) as a local file to run. Used at the *top* of
/// dispatch, before task lookup, so an explicit path always wins over a
/// same-named task.
///
/// Returns:
/// - `Ok(None)` — `token` is not a path-like local file (a bare name, an
///   existing directory, or a separator-bearing remote spec like
///   `@scope/pkg` or `github.com/owner/tool`); the caller continues normal
///   task / PM-exec resolution.
/// - `Ok(Some(_))` — `token` resolves to a runnable file; the caller spawns
///   the returned command instead of touching any package manager.
/// - `Err(_)` — `token` is unambiguously a local path that cannot be run
///   (a missing file behind an explicit `./`/`/`/`~` prefix, or a file of
///   unrecognized type); surfaced as a clear error rather than a 404.
pub(super) fn try_path_token(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    token: &str,
    args: &[String],
) -> Result<Option<LocalDispatch>> {
    if !is_path_like(token) {
        return Ok(None);
    }
    let path = resolve_path(token);
    dispatch_for_path(ctx, overrides, token, &path, args)
}

/// Try to interpret a *bare* `token` (no separator) as a runnable file in the
/// working directory. Used as a fallback *after* task lookup misses but
/// before the PM-exec fallback, so a bare name resolves to a task first
/// (never colliding) yet a local script such as `main.ts` still runs instead
/// of being mistaken for a remote package.
///
/// Only intercepts when the file is actually runnable (executable bit, `#!`
/// shebang, or a recognized source extension); a non-script bare token is
/// left to the PM-exec fallback so existing behavior is unchanged.
pub(super) fn try_bare_file(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    token: &str,
    args: &[String],
) -> Option<LocalDispatch> {
    if is_path_like(token) {
        return None;
    }
    let base = std::env::current_dir().ok()?;
    bare_file_in(ctx, overrides, &base, token, args)
}

/// Resolve a bare `token` against an explicit `base` directory. Split from
/// [`try_bare_file`] so the lookup can be unit-tested against a temp
/// directory without mutating the process working directory.
fn bare_file_in(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    base: &Path,
    token: &str,
    args: &[String],
) -> Option<LocalDispatch> {
    let path = base.join(token);
    let meta = fs::metadata(&path).ok()?;
    if !meta.is_file() {
        return None;
    }
    build_command(ctx, overrides, &path, &meta, args)
        .ok()
        .map(|(label, command)| LocalDispatch { label, command })
}

/// Resolve an already-path-like `token` (canonicalized to `path`) into a
/// dispatch. Split from [`try_path_token`] so the filesystem-dependent logic
/// can be unit-tested against temp files with absolute paths.
fn dispatch_for_path(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    token: &str,
    path: &Path,
    args: &[String],
) -> Result<Option<LocalDispatch>> {
    let Ok(meta) = fs::metadata(path) else {
        // The path resolves to nothing on disk. An explicit local path
        // (`./x`, `../x`, `/x`, `~/x`, `C:\x`) clearly means a file that
        // is not there — report it plainly. Anything else (`@scope/pkg`,
        // `github.com/owner/tool`) falls through so the PM-exec / Go
        // import-path fallback can treat it as a remote spec.
        if has_local_prefix(token) {
            bail!("no such file: {token}");
        }
        return Ok(None);
    };

    // Directories are never scripts: leave `./cmd/foo` to downstream
    // handlers such as `go run ./cmd/foo`. Other non-regular files (FIFOs,
    // sockets, devices) are not runnable here either.
    if !meta.is_file() {
        return Ok(None);
    }

    let (label, command) = build_command(ctx, overrides, path, &meta, args)?;
    Ok(Some(LocalDispatch { label, command }))
}

/// Build the spawn command for an existing regular file at `path`.
fn build_command(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    path: &Path,
    meta: &fs::Metadata,
    args: &[String],
) -> Result<(String, Command)> {
    // 1. Executable file → spawn directly. On Unix the kernel reads any
    //    `#!` shebang itself; on Windows this covers native `.exe`/`.com`.
    if is_directly_executable(path, meta) {
        let mut command = Command::new(path);
        command.args(args);
        return Ok((String::from("exec"), command));
    }

    // 2. A `#!` shebang names the interpreter explicitly — honor it even
    //    without the executable bit (and on Windows, which has none).
    if let Some(shebang) = read_shebang(path)? {
        return Ok(shebang_command(&shebang, path, args));
    }

    // 3. A recognized source extension runs via the project runtime.
    if let Some(runtime) = runtime_for_extension(ctx, overrides, path) {
        let (label, command) = command_for_runtime(runtime, path, args);
        return Ok((label.to_string(), command));
    }

    // 4. Out of options: a clear, actionable error (never a 404).
    bail!(
        "don't know how to run {}: it is not executable, has no `#!` shebang, and has no \
         recognized source extension.\nhint: add a shebang, mark it executable (chmod +x), or \
         give it a known extension (.ts/.tsx/.js/.mjs/.cjs/.py/.go).",
        path.display(),
    );
}

/// Whether `token` looks like a filesystem path rather than a bare task
/// name. A path separator (`/`, or `\` on Windows-style paths) or a leading
/// `~` is enough; bare names (`build`, `@scope/pkg` is handled later) never
/// enter the local-file branch, so they cannot collide with task names.
fn is_path_like(token: &str) -> bool {
    token.contains('/') || token.contains('\\') || token.starts_with('~')
}

/// Whether `token` carries an explicit local-path prefix. Used to decide
/// whether a *missing* path-like token is a typo'd local file (→ a clear
/// error) or a remote spec like `@scope/pkg` (→ fall through to PM-exec).
fn has_local_prefix(token: &str) -> bool {
    token.starts_with("./")
        || token.starts_with("../")
        || token.starts_with(".\\")
        || token.starts_with("..\\")
        || token.starts_with('/')
        || token.starts_with('\\')
        || token.starts_with('~')
        || is_windows_drive_abs(token)
}

/// Whether `token` starts with a Windows drive-letter root (`C:\` / `C:/`).
fn is_windows_drive_abs(token: &str) -> bool {
    let bytes = token.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'/' || bytes[2] == b'\\')
}

/// Resolve `token` to an absolute path: expand a leading `~`, then anchor a
/// relative path on the process working directory (where the user typed the
/// command). An absolute path is passed to the spawned command verbatim so
/// the child's working directory cannot reinterpret it.
fn resolve_path(token: &str) -> PathBuf {
    let expanded = crate::expand_tilde(Path::new(token));
    if expanded.is_absolute() {
        return expanded;
    }
    match std::env::current_dir() {
        Ok(cwd) => cwd.join(expanded),
        Err(_) => expanded,
    }
}

/// Whether the OS can execute `path` directly without an explicit
/// interpreter: the executable bit on Unix, a native executable extension
/// on Windows.
fn is_directly_executable(path: &Path, meta: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = path;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(windows)]
    {
        let _ = meta;
        matches!(ext_lower(path).as_deref(), Some("exe" | "com"))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (path, meta);
        false
    }
}

/// Lower-cased file extension of `path`, if any.
fn ext_lower(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase)
}

/// Parsed `#!` interpreter line.
#[derive(Debug, PartialEq, Eq)]
struct Shebang {
    program: String,
    args: Vec<String>,
}

/// Read the first line of `path` and parse a `#!` shebang, if present.
///
/// Reads a bounded prefix (a shebang line is short) so a binary file never
/// pulls a large read; a non-UTF-8 shebang line is treated as no shebang.
fn read_shebang(path: &Path) -> Result<Option<Shebang>> {
    use std::io::Read as _;

    let mut file = fs::File::open(path)
        .with_context(|| format!("opening {} to read its shebang", path.display()))?;
    let mut buf = [0u8; 256];
    let read = file
        .read(&mut buf)
        .with_context(|| format!("reading {}", path.display()))?;
    let head = &buf[..read];
    if !head.starts_with(b"#!") {
        return Ok(None);
    }
    let line_end = head
        .iter()
        .position(|&byte| byte == b'\n')
        .unwrap_or(head.len());
    let Ok(line) = std::str::from_utf8(&head[..line_end]) else {
        return Ok(None);
    };
    Ok(parse_shebang(line))
}

/// Parse a `#!` line into the interpreter program and its arguments.
///
/// Handles the `/usr/bin/env [-S|--split-string[=]] <interp> [args...]`
/// form: the kernel passes everything after `env ` as a single argument, so
/// a `-S` flag re-splits it. We have already split on whitespace, so we only
/// need to drop the flag and treat the next token as the real interpreter.
fn parse_shebang(line: &str) -> Option<Shebang> {
    let body = line.strip_prefix("#!")?.trim();
    if body.is_empty() {
        return None;
    }
    let (interpreter, rest) = match body.split_once(char::is_whitespace) {
        Some((interp, rest)) => (interp, rest.trim()),
        None => (body, ""),
    };

    if is_env(interpreter) {
        let command = rest
            .strip_prefix("--split-string=")
            .or_else(|| rest.strip_prefix("--split-string"))
            .or_else(|| rest.strip_prefix("-S"))
            .unwrap_or(rest)
            .trim();
        let mut parts = command.split_whitespace();
        let program = parts.next()?.to_string();
        let args = parts.map(ToOwned::to_owned).collect();
        return Some(Shebang { program, args });
    }

    let args = rest.split_whitespace().map(ToOwned::to_owned).collect();
    Some(Shebang {
        program: interpreter.to_string(),
        args,
    })
}

/// Whether `interpreter` is the `env` launcher (by file name).
fn is_env(interpreter: &str) -> bool {
    Path::new(interpreter)
        .file_name()
        .is_some_and(|name| name == "env")
}

/// Build the command for a shebang-described file: `<interp> [interp-args]
/// <file> [forwarded-args]`.
fn shebang_command(shebang: &Shebang, file: &Path, args: &[String]) -> (String, Command) {
    let mut command = tool::program::command(&shebang.program);
    command.args(&shebang.args).arg(file).args(args);
    (shebang_label(shebang), command)
}

/// Short trace label for a shebang interpreter (`deno run`, `python3`, …).
fn shebang_label(shebang: &Shebang) -> String {
    let program = Path::new(&shebang.program)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(shebang.program.as_str());
    if shebang.args.is_empty() {
        program.to_string()
    } else {
        format!("{program} {}", shebang.args.join(" "))
    }
}

/// A language runtime that executes a local source file by extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Runtime {
    Bun,
    Deno,
    Node,
    Uv,
    Python,
    Go,
    #[cfg(windows)]
    WindowsScript,
}

/// Map a file's extension to the runtime that should execute it, given the
/// detected project and any `--pm` override.
fn runtime_for_extension(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    path: &Path,
) -> Option<Runtime> {
    match ext_lower(path)?.as_str() {
        "ts" | "tsx" | "mts" | "cts" | "js" | "jsx" | "mjs" | "cjs" => {
            Some(js_runtime(ctx, overrides))
        }
        "py" => Some(py_runtime(ctx, overrides)),
        "go" => Some(Runtime::Go),
        #[cfg(windows)]
        "ps1" | "bat" | "cmd" => Some(Runtime::WindowsScript),
        _ => None,
    }
}

/// JavaScript/TypeScript runtime: an explicit JS-capable `--pm`/config
/// override wins, then the detected project runtime (Deno or Bun are
/// runtimes in their own right), defaulting to Node.
fn js_runtime(ctx: &ProjectContext, overrides: &ResolutionOverrides) -> Runtime {
    if let Some(runtime) = js_runtime_from_override(overrides) {
        return runtime;
    }
    if ctx.package_managers.contains(&PackageManager::Deno) {
        return Runtime::Deno;
    }
    if ctx.package_managers.contains(&PackageManager::Bun) {
        return Runtime::Bun;
    }
    Runtime::Node
}

/// Resolve a JS runtime from an explicit override (`--pm`, `RUNNER_PM`, or
/// `runner.toml` `[pm].node`/`[pm].deno`). A non-JS override (e.g.
/// `--pm cargo`) yields `None` so detection decides instead.
fn js_runtime_from_override(overrides: &ResolutionOverrides) -> Option<Runtime> {
    let pm = overrides
        .pm
        .as_ref()
        .map(|over| over.pm)
        .or_else(|| {
            overrides
                .pm_by_ecosystem
                .get(&Ecosystem::Node)
                .map(|over| over.pm)
        })
        .or_else(|| {
            overrides
                .pm_by_ecosystem
                .get(&Ecosystem::Deno)
                .map(|over| over.pm)
        })?;
    match pm {
        PackageManager::Deno => Some(Runtime::Deno),
        PackageManager::Bun => Some(Runtime::Bun),
        node if node.is_node() => Some(Runtime::Node),
        _ => None,
    }
}

/// Python runtime: `uv run` when uv is overridden or detected, otherwise the
/// plain interpreter. A non-uv Python override (poetry/pipenv) uses the
/// plain interpreter — `uv run` would be wrong for those projects.
fn py_runtime(ctx: &ProjectContext, overrides: &ResolutionOverrides) -> Runtime {
    let overridden = overrides.pm.as_ref().map(|over| over.pm).or_else(|| {
        overrides
            .pm_by_ecosystem
            .get(&Ecosystem::Python)
            .map(|over| over.pm)
    });
    if let Some(pm) = overridden {
        if pm == PackageManager::Uv {
            return Runtime::Uv;
        }
        if pm.ecosystem() == Ecosystem::Python {
            return Runtime::Python;
        }
    }
    if ctx.package_managers.contains(&PackageManager::Uv) {
        Runtime::Uv
    } else {
        Runtime::Python
    }
}

/// Build the command (and trace label) for a runtime running `file`.
fn command_for_runtime(runtime: Runtime, file: &Path, args: &[String]) -> (&'static str, Command) {
    match runtime {
        Runtime::Bun => ("bun", tool::bun::run_file_cmd(file, args)),
        Runtime::Deno => ("deno run", tool::deno::run_file_cmd(file, args)),
        Runtime::Node => ("node", tool::node::run_file_cmd(file, args)),
        Runtime::Uv => ("uv run", tool::uv::run_file_cmd(file, args)),
        Runtime::Python => (
            tool::python::PYTHON_BIN,
            tool::python::run_file_cmd(file, args),
        ),
        Runtime::Go => ("go run", tool::go_pm::run_file_cmd(file, args)),
        #[cfg(windows)]
        Runtime::WindowsScript => windows_script_command(file, args),
    }
}

/// Build the command for a Windows native script: `.ps1` via PowerShell,
/// `.bat`/`.cmd` via `cmd /c` (neither is launchable through
/// `CreateProcess` directly).
#[cfg(windows)]
fn windows_script_command(file: &Path, args: &[String]) -> (&'static str, Command) {
    if ext_lower(file).as_deref() == Some("ps1") {
        let mut command = tool::program::command("powershell");
        command
            .arg("-NoProfile")
            .arg("-ExecutionPolicy")
            .arg("Bypass")
            .arg("-File")
            .arg(file)
            .args(args);
        ("powershell", command)
    } else {
        let mut command = tool::program::command("cmd");
        command.arg("/c").arg(file).args(args);
        ("cmd", command)
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{
        Runtime, bare_file_in, build_command, dispatch_for_path, has_local_prefix, is_path_like,
        js_runtime, parse_shebang, py_runtime, runtime_for_extension, try_bare_file,
        try_path_token,
    };
    use crate::resolver::ResolutionOverrides;
    use crate::tool::test_support::TempDir;
    use crate::types::{PackageManager, ProjectContext};

    fn context(pms: Vec<PackageManager>) -> ProjectContext {
        ProjectContext {
            root: PathBuf::from("."),
            package_managers: pms,
            task_runners: Vec::new(),
            tasks: Vec::new(),
            node_version: None,
            current_node: None,
            is_monorepo: false,
            warnings: Vec::new(),
        }
    }

    fn args_of(command: &std::process::Command) -> Vec<String> {
        command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn is_path_like_requires_a_separator_or_tilde() {
        assert!(is_path_like("bin/recipe.ts"));
        assert!(is_path_like("./build.sh"));
        assert!(is_path_like("../tools/x.js"));
        assert!(is_path_like("/abs/path"));
        assert!(is_path_like("~/script.ts"));
        assert!(is_path_like(r"dir\file.ts"));
        // Bare names (including scoped tasks) are not path-like.
        assert!(!is_path_like("build"));
        assert!(!is_path_like("lint:cargo"));
        assert!(!is_path_like("test"));
    }

    #[test]
    fn has_local_prefix_distinguishes_paths_from_remote_specs() {
        assert!(has_local_prefix("./x"));
        assert!(has_local_prefix("../x"));
        assert!(has_local_prefix("/x"));
        assert!(has_local_prefix("~/x"));
        assert!(has_local_prefix(r".\x"));
        assert!(has_local_prefix(r"C:\x"));
        // Remote specs that merely contain a separator must not look local.
        assert!(!has_local_prefix("@scope/pkg"));
        assert!(!has_local_prefix("github.com/owner/tool"));
        assert!(!has_local_prefix("bin/x.ts"));
    }

    #[test]
    fn parse_shebang_plain_interpreter() {
        let parsed = parse_shebang("#!/bin/sh").expect("shebang should parse");
        assert_eq!(parsed.program, "/bin/sh");
        assert!(parsed.args.is_empty());
    }

    #[test]
    fn parse_shebang_interpreter_with_args() {
        let parsed = parse_shebang("#!/usr/bin/python3 -O").expect("shebang should parse");
        assert_eq!(parsed.program, "/usr/bin/python3");
        assert_eq!(parsed.args, ["-O"]);
    }

    #[test]
    fn parse_shebang_env_resolves_real_interpreter() {
        let parsed = parse_shebang("#!/usr/bin/env python3").expect("shebang should parse");
        assert_eq!(parsed.program, "python3");
        assert!(parsed.args.is_empty());
    }

    #[test]
    fn parse_shebang_env_split_string_forms() {
        for line in [
            "#!/usr/bin/env -S deno run -A",
            "#!/usr/bin/env --split-string=deno run -A",
            "#!/usr/bin/env --split-string deno run -A",
            "#!/usr/bin/env -Sdeno run -A",
        ] {
            let parsed = parse_shebang(line).unwrap_or_else(|| panic!("should parse {line:?}"));
            assert_eq!(parsed.program, "deno", "program from {line:?}");
            assert_eq!(parsed.args, ["run", "-A"], "args from {line:?}");
        }
    }

    #[test]
    fn parse_shebang_rejects_non_shebang() {
        assert!(parse_shebang("not a shebang").is_none());
        assert!(parse_shebang("#!").is_none());
        assert!(parse_shebang("#!   ").is_none());
    }

    #[test]
    fn js_runtime_follows_detected_project() {
        let defaults = ResolutionOverrides::default();
        assert_eq!(
            js_runtime(&context(vec![PackageManager::Deno]), &defaults),
            Runtime::Deno,
        );
        assert_eq!(
            js_runtime(&context(vec![PackageManager::Bun]), &defaults),
            Runtime::Bun,
        );
        assert_eq!(
            js_runtime(&context(vec![PackageManager::Pnpm]), &defaults),
            Runtime::Node,
        );
        assert_eq!(js_runtime(&context(vec![]), &defaults), Runtime::Node);
    }

    #[test]
    fn py_runtime_prefers_uv_when_detected() {
        let defaults = ResolutionOverrides::default();
        assert_eq!(
            py_runtime(&context(vec![PackageManager::Uv]), &defaults),
            Runtime::Uv,
        );
        assert_eq!(
            py_runtime(&context(vec![PackageManager::Poetry]), &defaults),
            Runtime::Python,
        );
        assert_eq!(py_runtime(&context(vec![]), &defaults), Runtime::Python);
    }

    #[test]
    fn runtime_for_extension_maps_known_sources() {
        let ctx = context(vec![PackageManager::Bun]);
        let defaults = ResolutionOverrides::default();
        assert_eq!(
            runtime_for_extension(&ctx, &defaults, Path::new("a.ts")),
            Some(Runtime::Bun),
        );
        assert_eq!(
            runtime_for_extension(&ctx, &defaults, Path::new("a.go")),
            Some(Runtime::Go),
        );
        assert_eq!(
            runtime_for_extension(&ctx, &defaults, Path::new("a.py")),
            Some(Runtime::Python),
        );
        assert_eq!(
            runtime_for_extension(&ctx, &defaults, Path::new("a.txt")),
            None,
        );
    }

    #[test]
    fn build_command_runs_ts_via_detected_runtime() {
        let dir = TempDir::new("local-ts");
        let file = dir.path().join("script.ts");
        std::fs::write(&file, "console.log('hi')\n").expect("file should be written");
        let meta = std::fs::metadata(&file).expect("metadata should read");

        let (label, command) = build_command(
            &context(vec![PackageManager::Bun]),
            &ResolutionOverrides::default(),
            &file,
            &meta,
            &[String::from("--flag")],
        )
        .expect("ts file should build a command");

        assert_eq!(label, "bun");
        assert_eq!(command.get_program().to_string_lossy(), "bun");
        assert_eq!(
            args_of(&command),
            [file.to_string_lossy().into_owned(), String::from("--flag")],
        );
    }

    #[test]
    fn build_command_runs_py_via_uv_when_detected() {
        let dir = TempDir::new("local-py");
        let file = dir.path().join("task.py");
        std::fs::write(&file, "print('hi')\n").expect("file should be written");
        let meta = std::fs::metadata(&file).expect("metadata should read");

        let (label, command) = build_command(
            &context(vec![PackageManager::Uv]),
            &ResolutionOverrides::default(),
            &file,
            &meta,
            &[],
        )
        .expect("py file should build a command");

        assert_eq!(label, "uv run");
        assert_eq!(command.get_program().to_string_lossy(), "uv");
        assert_eq!(
            args_of(&command),
            [String::from("run"), file.to_string_lossy().into_owned()],
        );
    }

    #[test]
    fn build_command_honors_shebang_without_exec_bit() {
        let dir = TempDir::new("local-shebang");
        let file = dir.path().join("noexec.sh");
        std::fs::write(&file, "#!/usr/bin/env -S deno run -A\nconsole.log(1)\n")
            .expect("file should be written");
        let meta = std::fs::metadata(&file).expect("metadata should read");

        let (label, command) = build_command(
            &context(vec![PackageManager::Bun]),
            &ResolutionOverrides::default(),
            &file,
            &meta,
            &[String::from("x")],
        )
        .expect("shebang file should build a command");

        // The shebang wins over the extension/runtime default (Bun here).
        assert_eq!(label, "deno run -A");
        assert_eq!(command.get_program().to_string_lossy(), "deno");
        assert_eq!(
            args_of(&command),
            [
                String::from("run"),
                String::from("-A"),
                file.to_string_lossy().into_owned(),
                String::from("x"),
            ],
        );
    }

    #[test]
    fn build_command_errors_on_unrunnable_file() {
        let dir = TempDir::new("local-unknown");
        let file = dir.path().join("data.bin");
        std::fs::write(&file, [0u8, 1, 2, 3]).expect("file should be written");
        let meta = std::fs::metadata(&file).expect("metadata should read");

        let err = build_command(
            &context(vec![]),
            &ResolutionOverrides::default(),
            &file,
            &meta,
            &[],
        )
        .expect_err("a non-runnable file should error");

        assert!(format!("{err:#}").contains("don't know how to run"));
    }

    #[cfg(unix)]
    #[test]
    fn build_command_spawns_executable_directly() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = TempDir::new("local-exec");
        let file = dir.path().join("build.sh");
        std::fs::write(&file, "#!/bin/sh\necho hi\n").expect("file should be written");
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755))
            .expect("chmod should succeed");
        let meta = std::fs::metadata(&file).expect("metadata should read");

        let (label, command) = build_command(
            &context(vec![]),
            &ResolutionOverrides::default(),
            &file,
            &meta,
            &[String::from("arg")],
        )
        .expect("executable should build a command");

        assert_eq!(label, "exec");
        assert_eq!(
            PathBuf::from(command.get_program()),
            file,
            "an executable file is spawned by its own path",
        );
        assert_eq!(args_of(&command), [String::from("arg")]);
    }

    #[test]
    fn dispatch_for_path_passes_through_directories() {
        let dir = TempDir::new("local-dir");
        let result = dispatch_for_path(
            &context(vec![]),
            &ResolutionOverrides::default(),
            "./some-dir",
            dir.path(),
            &[],
        )
        .expect("a directory should not error");

        assert!(result.is_none(), "directories fall through, not run");
    }

    #[test]
    fn dispatch_for_path_errors_on_missing_explicit_path() {
        let dir = TempDir::new("local-missing");
        let missing = dir.path().join("ghost.ts");

        let err = dispatch_for_path(
            &context(vec![]),
            &ResolutionOverrides::default(),
            "./ghost.ts",
            &missing,
            &[],
        )
        .expect_err("an explicit missing path should error");

        assert!(format!("{err:#}").contains("no such file"));
    }

    #[test]
    fn dispatch_for_path_passes_through_missing_remote_spec() {
        let dir = TempDir::new("local-remote");
        let missing = dir.path().join("biome");

        let result = dispatch_for_path(
            &context(vec![]),
            &ResolutionOverrides::default(),
            "@biomejs/biome",
            &missing,
            &[],
        )
        .expect("a remote spec should not error");

        assert!(
            result.is_none(),
            "a separator-bearing remote spec falls through to PM-exec",
        );
    }

    #[test]
    fn try_path_token_ignores_bare_names() {
        let result = try_path_token(
            &context(vec![PackageManager::Bun]),
            &ResolutionOverrides::default(),
            "test",
            &[],
        )
        .expect("bare names should not error");

        assert!(
            result.is_none(),
            "bare names are handled by the bare-file fallback, not the path branch"
        );
    }

    #[test]
    fn bare_file_runs_a_runnable_file_in_base_dir() {
        // A bare `main.ts` in the base directory dispatches via the runtime
        // instead of falling through to package-exec.
        let dir = TempDir::new("bare-file");
        std::fs::write(dir.path().join("main.ts"), "console.log(1)\n")
            .expect("file should be written");

        let dispatch = bare_file_in(
            &context(vec![PackageManager::Deno]),
            &ResolutionOverrides::default(),
            dir.path(),
            "main.ts",
            &[],
        )
        .expect("a runnable bare file should dispatch");

        assert_eq!(dispatch.label, "deno run");
        assert_eq!(dispatch.command.get_program().to_string_lossy(), "deno");
    }

    #[test]
    fn bare_file_ignores_missing_and_non_script_tokens() {
        let dir = TempDir::new("bare-file-miss");
        std::fs::write(dir.path().join("data.bin"), [0u8, 1, 2]).expect("file should be written");

        let ctx = context(vec![PackageManager::Bun]);
        let defaults = ResolutionOverrides::default();

        // A bare token with no matching file falls through to PM-exec.
        assert!(bare_file_in(&ctx, &defaults, dir.path(), "biome", &[]).is_none());
        // A file we don't know how to run is left to PM-exec too.
        assert!(bare_file_in(&ctx, &defaults, dir.path(), "data.bin", &[]).is_none());
    }

    #[test]
    fn try_bare_file_ignores_path_like_tokens() {
        // Separator-bearing tokens are the path branch's responsibility;
        // the bare-file fallback must decline them outright.
        assert!(
            try_bare_file(
                &context(vec![PackageManager::Bun]),
                &ResolutionOverrides::default(),
                "./main.ts",
                &[],
            )
            .is_none()
        );
    }
}
