//! Local-file execution for `run <path>`.
//!
//! A token that points at a local file should be *run as that file* — never
//! handed to a package manager's package-exec primitive (`bunx`/`npx`/
//! `pnpm dlx`/`deno x`/`uvx`), which would resolve a local path as a remote
//! package spec and fail with a registry 404 or a `git clone` error.
//!
//! [`try_path_token`] runs at the top of [`super::dispatch::resolve_dispatch`]
//! (an explicit-prefix path, before task lookup), and [`try_bare_file`] runs
//! after a task miss but before the PM-exec fallback (a bare or relative
//! token). Each classifies a path-like token into one of four outcomes:
//!
//! 1. **Directly executable file** (a native binary, or a script whose `#!`
//!    line the kernel can honor) → spawned directly (`Command::new(path)`).
//!    A recognized *source* file that merely carries the exec bit but has no
//!    `#!` line is **not** run this way — `execve` cannot run shebang-less
//!    text (it returns `ENOEXEC`) — it falls to outcome 3 instead.
//! 2. **Non-executable file with a `#!` shebang** → the interpreter is
//!    parsed (including `#!/usr/bin/env -S <interp> <args>`) and the file is
//!    run through it.
//! 3. **Recognized source extension** → run via the project runtime
//!    (`.ts`/`.js`/… → bun / `deno run` / node; `.py` → `uv run` / python;
//!    `.go` → `go run`), *not* package-exec. Reached even with the exec bit
//!    set, since a shebang-less source file is not a native executable.
//! 4. **Otherwise** → a clear, actionable error.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Result, bail};

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

/// Try to interpret a `token` carrying an explicit local-path prefix
/// (`./`, `../`, `/`, `\`, `~`, or a Windows drive root) as a local file to
/// run. Used at the *top* of dispatch, before task lookup, so an explicit
/// path always wins over a same-named task.
///
/// Only an explicit prefix outranks a task here. A separator-bearing but
/// *relative* token such as `bin/tool` is left for the after-task-miss
/// [`try_bare_file`] fallback, so a matching task (e.g. a `make bin/tool`
/// target) wins first and a built artifact on disk cannot silently shadow it.
///
/// Returns:
/// - `Ok(None)` — `token` has no local prefix (a bare name, a relative
///   `bin/tool`, an existing directory, or a remote spec like `@scope/pkg`
///   or `github.com/owner/tool`); the caller continues normal task /
///   PM-exec resolution.
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
    if !has_local_prefix(token) {
        return Ok(None);
    }
    let path = resolve_path(&ctx.root, token);
    dispatch_for_path(ctx, overrides, token, &path, args)
}

/// Try to interpret a `token` *without* an explicit local-path prefix — a
/// bare name (`main.ts`) or a relative path bearing a separator
/// (`bin/tool`) — as a runnable file under the project root (`ctx.root`).
/// Used as a
/// fallback *after* task lookup misses but before the PM-exec fallback, so a
/// matching task always wins first (a bare name or a `make bin/tool` target
/// never collides) yet a local script such as `main.ts` still runs instead
/// of being mistaken for a remote package.
///
/// Prefix-bearing paths (`./x`, `/x`, `~/x`, `C:\x`) are the pre-task
/// [`try_path_token`] branch's job and are declined here. Only intercepts
/// when the file is actually runnable (executable bit, `#!` shebang, or a
/// recognized source extension); a non-script token is left to the PM-exec
/// fallback so existing behavior is unchanged.
pub(super) fn try_bare_file(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    token: &str,
    args: &[String],
) -> Option<LocalDispatch> {
    if has_local_prefix(token) {
        return None;
    }
    bare_file_in(ctx, overrides, &ctx.root, token, args)
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
    // Read any `#!` shebang once up front: it is what disambiguates an
    // exec-bit-carrying *source* file (a `+x` `.ts` with no shebang is still
    // text that `execve` cannot run) from a genuine self-executable script
    // or native binary. An execute-only file (mode 0111) cannot be opened for
    // read, so the probe simply yields `None` (no shebang to honor) rather
    // than aborting — the kernel can still `execve` such a binary.
    let shebang = read_shebang(path);
    let routing = routing_for_extension(ctx, overrides, path);

    // 1. Directly executable → spawn directly, *unless* it is a recognized
    //    source file with no `#!` line. On Unix the kernel honors a real
    //    shebang itself; on Windows this covers native `.exe`/`.com`. A
    //    shebang-less source file with the exec bit (common on
    //    vfat/exfat/ntfs-3g mounts that report mode 0777 for every file)
    //    would otherwise hit `execve`, fail `ENOEXEC`, and never reach
    //    bun/deno/node/uv/python/go — so route it to the runtime (outcome 3).
    if is_directly_executable(path, meta)
        && (shebang.is_some() || matches!(routing, SourceRouting::Unrecognized))
    {
        let mut command = Command::new(path);
        command.args(args);
        return Ok((String::from("exec"), command));
    }

    // 2. A `#!` shebang names the interpreter explicitly — honor it even
    //    without the executable bit (and on Windows, which has none).
    if let Some(shebang) = shebang {
        return Ok(shebang_command(&shebang, path, args));
    }

    // 3. A recognized source extension runs via the project runtime — except
    //    a `.jsx`/`.tsx` file resolved to Node, which Node cannot execute
    //    (no JSX transform; type-stripping covers only `.ts`/`.mts`/`.cts`).
    //    Erroring is honest; `node app.tsx` would be guaranteed-broken.
    match routing {
        SourceRouting::Runtime(runtime) => {
            let (label, command) = command_for_runtime(runtime, path, args);
            return Ok((label.to_string(), command));
        }
        SourceRouting::NodeCannotRunJsx => bail!(
            "node cannot run {}: Node has no JSX/TSX transform (it type-strips only \
             .ts/.mts/.cts).\nhint: run it with bun or deno (a Bun/Deno project, or pass `--pm \
             bun`/`--pm deno`).",
            path.display(),
        ),
        SourceRouting::Unrecognized => {}
    }

    // 4. Out of options: a clear, actionable error (never a 404).
    bail!(
        "don't know how to run {}: it is not executable, has no `#!` shebang, and has no \
         recognized source extension.\nhint: add a shebang, mark it executable (chmod +x), or \
         give it a known extension (.ts/.tsx/.js/.mjs/.cjs/.py/.go).",
        path.display(),
    );
}

/// Whether `token` carries an explicit local-path prefix. Used to decide
/// whether a *missing* path-like token is a typo'd local file (→ a clear
/// error) or a remote spec like `@scope/pkg` (→ fall through to PM-exec).
///
/// Exposed to the rest of `cmd::run` so [`super::qualify::precheck_task`]
/// can mirror [`try_path_token`]'s precedence: an explicit-prefix path
/// outranks task/runner resolution, so precheck must wave it through
/// rather than fail it against an active `--runner`/`[task_runner].prefer`
/// constraint (which dispatch never applies to a local-file token).
pub(super) fn has_local_prefix(token: &str) -> bool {
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
/// relative path on `base` — the detected project root (`ctx.root`), which
/// also becomes the spawned child's working directory and is what task
/// detection runs against. Anchoring here (rather than on the live process
/// cwd) keeps local-file lookup consistent with every other dispatch step,
/// so `--dir`/`RUNNER_DIR` points the path lookup at the same directory the
/// child will run in. An absolute path (or a `~`-expanded one) is passed to
/// the spawned command verbatim so the child's working directory cannot
/// reinterpret it.
fn resolve_path(base: &Path, token: &str) -> PathBuf {
    let expanded = crate::expand_tilde(Path::new(token));
    if expanded.is_absolute() {
        return expanded;
    }
    base.join(expanded)
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
///
/// I/O failure is non-fatal and yields `None`: by the time this runs the path
/// is already a regular file (`fs::metadata` succeeded), so a failed
/// `open`/`read` means an execute-only file (Unix mode 0111 — a legitimate
/// mode for compiled binaries) that the kernel can `execve` but cannot be
/// opened `O_RDONLY` (EACCES). "No shebang we can honor" → defer to the exec
/// bit / extension rather than hard-failing the whole dispatch.
fn read_shebang(path: &Path) -> Option<Shebang> {
    use std::io::Read as _;

    let Ok(mut file) = fs::File::open(path) else {
        return None;
    };
    let mut buf = [0u8; 256];
    let Ok(read) = file.read(&mut buf) else {
        return None;
    };
    let head = &buf[..read];
    if !head.starts_with(b"#!") {
        return None;
    }
    let line_end = head
        .iter()
        .position(|&byte| byte == b'\n')
        .unwrap_or(head.len());
    let Ok(line) = std::str::from_utf8(&head[..line_end]) else {
        return None;
    };
    parse_shebang(line)
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

/// How a local file's extension routes for execution.
#[derive(Debug, PartialEq, Eq)]
enum SourceRouting {
    /// Run the file through this language runtime (outcome 3).
    Runtime(Runtime),
    /// A `.jsx`/`.tsx` file resolved to the Node runtime, which cannot
    /// execute JSX/TSX: Node has no JSX transform and type-strips only
    /// `.ts`/`.mts`/`.cts`. A clear error, never a broken `node app.tsx`.
    NodeCannotRunJsx,
    /// Not a recognized source extension — defer to the exec bit / shebang
    /// (a native binary) or, failing that, outcome 4's error.
    Unrecognized,
}

/// Map a file's extension to how it should be executed, given the detected
/// project and any `--pm` override.
fn routing_for_extension(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    path: &Path,
) -> SourceRouting {
    let Some(ext) = ext_lower(path) else {
        return SourceRouting::Unrecognized;
    };
    match ext.as_str() {
        "ts" | "mts" | "cts" | "js" | "mjs" | "cjs" => {
            SourceRouting::Runtime(js_runtime(ctx, overrides))
        }
        // `.jsx`/`.tsx` need a JSX-aware runtime. Bun and Deno run them
        // directly; Node cannot (no JSX transform), so route those to a clear
        // error instead of building an unrunnable `node app.tsx`.
        "jsx" | "tsx" => match js_runtime(ctx, overrides) {
            runtime @ (Runtime::Bun | Runtime::Deno) => SourceRouting::Runtime(runtime),
            _ => SourceRouting::NodeCannotRunJsx,
        },
        "py" => SourceRouting::Runtime(py_runtime(ctx, overrides)),
        "go" => SourceRouting::Runtime(Runtime::Go),
        #[cfg(windows)]
        "ps1" | "bat" | "cmd" => SourceRouting::Runtime(Runtime::WindowsScript),
        _ => SourceRouting::Unrecognized,
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
        Runtime, SourceRouting, bare_file_in, build_command, dispatch_for_path, has_local_prefix,
        js_runtime, parse_shebang, py_runtime, routing_for_extension, try_bare_file,
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

    fn context_rooted(pms: Vec<PackageManager>, root: PathBuf) -> ProjectContext {
        ProjectContext {
            root,
            ..context(pms)
        }
    }

    fn args_of(command: &std::process::Command) -> Vec<String> {
        command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
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
    fn routing_for_extension_maps_known_sources() {
        let ctx = context(vec![PackageManager::Bun]);
        let defaults = ResolutionOverrides::default();
        assert_eq!(
            routing_for_extension(&ctx, &defaults, Path::new("a.ts")),
            SourceRouting::Runtime(Runtime::Bun),
        );
        assert_eq!(
            routing_for_extension(&ctx, &defaults, Path::new("a.go")),
            SourceRouting::Runtime(Runtime::Go),
        );
        assert_eq!(
            routing_for_extension(&ctx, &defaults, Path::new("a.py")),
            SourceRouting::Runtime(Runtime::Python),
        );
        assert_eq!(
            routing_for_extension(&ctx, &defaults, Path::new("a.txt")),
            SourceRouting::Unrecognized,
        );
    }

    #[test]
    fn routing_for_extension_routes_jsx_tsx_to_bun_or_deno() {
        let defaults = ResolutionOverrides::default();
        for ext in ["a.jsx", "a.tsx"] {
            assert_eq!(
                routing_for_extension(
                    &context(vec![PackageManager::Bun]),
                    &defaults,
                    Path::new(ext),
                ),
                SourceRouting::Runtime(Runtime::Bun),
                "{ext} should run via bun in a bun project",
            );
            assert_eq!(
                routing_for_extension(
                    &context(vec![PackageManager::Deno]),
                    &defaults,
                    Path::new(ext),
                ),
                SourceRouting::Runtime(Runtime::Deno),
                "{ext} should run via deno in a deno project",
            );
        }
    }

    #[test]
    fn routing_for_extension_rejects_jsx_tsx_on_node() {
        // Node has no JSX transform: `node app.jsx`/`node app.tsx` are
        // categorically unrunnable, so a node-only (or no-PM) project routes
        // them to a clear error rather than an unrunnable `node` command.
        let defaults = ResolutionOverrides::default();
        for ext in ["a.jsx", "a.tsx"] {
            assert_eq!(
                routing_for_extension(
                    &context(vec![PackageManager::Pnpm]),
                    &defaults,
                    Path::new(ext),
                ),
                SourceRouting::NodeCannotRunJsx,
                "{ext} must not route to bare node",
            );
            assert_eq!(
                routing_for_extension(&context(vec![]), &defaults, Path::new(ext)),
                SourceRouting::NodeCannotRunJsx,
                "{ext} must not route to bare node in a no-PM project",
            );
        }
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

    #[test]
    fn build_command_errors_on_jsx_in_node_project() {
        // A node-only project building `node app.tsx` would be guaranteed
        // broken (Node has no JSX transform). It must surface a clear error
        // rather than an unrunnable command.
        let dir = TempDir::new("local-tsx-node");
        let file = dir.path().join("app.tsx");
        std::fs::write(&file, "export const App = () => <div/>;\n")
            .expect("file should be written");
        let meta = std::fs::metadata(&file).expect("metadata should read");

        let err = build_command(
            &context(vec![PackageManager::Pnpm]),
            &ResolutionOverrides::default(),
            &file,
            &meta,
            &[],
        )
        .expect_err("a .tsx in a node project should error, not build `node app.tsx`");

        assert!(format!("{err:#}").contains("node cannot run"));
    }

    #[test]
    fn build_command_runs_tsx_via_bun_when_detected() {
        // A bun project can run `.tsx` directly, so it dispatches via bun.
        let dir = TempDir::new("local-tsx-bun");
        let file = dir.path().join("app.tsx");
        std::fs::write(&file, "export const App = () => <div/>;\n")
            .expect("file should be written");
        let meta = std::fs::metadata(&file).expect("metadata should read");

        let (label, command) = build_command(
            &context(vec![PackageManager::Bun]),
            &ResolutionOverrides::default(),
            &file,
            &meta,
            &[],
        )
        .expect("a .tsx in a bun project should build a bun command");

        assert_eq!(label, "bun");
        assert_eq!(command.get_program().to_string_lossy(), "bun");
    }

    #[cfg(unix)]
    #[test]
    fn build_command_spawns_execute_only_binary_directly() {
        use std::os::unix::fs::PermissionsExt as _;

        // A native binary at mode 0111 (execute-only — a legitimate mode):
        // the kernel can `execve` it, but `open(O_RDONLY)` returns EACCES so
        // the shebang probe cannot read it. It must still spawn directly
        // (outcome 1) rather than hard-fail on the unreadable shebang read.
        let dir = TempDir::new("local-exec-only");
        let file = dir.path().join("tool");
        std::fs::write(&file, [0u8, 1, 2, 3]).expect("file should be written");
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o111))
            .expect("chmod should succeed");
        let meta = std::fs::metadata(&file).expect("metadata should read");

        let (label, command) = build_command(
            &context(vec![]),
            &ResolutionOverrides::default(),
            &file,
            &meta,
            &[String::from("arg")],
        )
        .expect("an execute-only binary should spawn directly, not fail the shebang read");

        assert_eq!(label, "exec");
        assert_eq!(PathBuf::from(command.get_program()), file);
        assert_eq!(args_of(&command), [String::from("arg")]);
    }

    #[cfg(unix)]
    #[test]
    fn build_command_routes_execute_only_source_to_runtime_not_pm_exec() {
        use std::os::unix::fs::PermissionsExt as _;

        // A recognized source file at mode 0111 (execute-only). The shebang
        // probe hits EACCES, but that must NOT propagate as an error (which
        // `bare_file_in`'s `.ok()` would swallow into a `bunx main.ts` 404).
        // It routes through the detected runtime instead, upholding the
        // no-bunx-on-a-local-file invariant.
        let dir = TempDir::new("local-exec-only-ts");
        let file = dir.path().join("main.ts");
        std::fs::write(&file, "console.log(1)\n").expect("file should be written");
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o111))
            .expect("chmod should succeed");
        let meta = std::fs::metadata(&file).expect("metadata should read");

        let (label, command) = build_command(
            &context(vec![PackageManager::Bun]),
            &ResolutionOverrides::default(),
            &file,
            &meta,
            &[],
        )
        .expect("an execute-only source file should route to its runtime");

        assert_eq!(label, "bun", "a 0111 .ts routes to bun, never bunx");
        assert_eq!(command.get_program().to_string_lossy(), "bun");
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

    #[cfg(unix)]
    #[test]
    fn build_command_runs_exec_bit_source_without_shebang_via_runtime() {
        use std::os::unix::fs::PermissionsExt as _;

        // A `.ts` file that carries the exec bit but has no `#!` shebang is
        // still text: a raw `execve` would fail `ENOEXEC` (or be retried as a
        // broken `/bin/sh` parse) and never reach bun. It must dispatch
        // through the detected runtime instead. This is the real breakage on
        // vfat/exfat/ntfs-3g mounts that report mode 0777 for every file.
        let dir = TempDir::new("local-exec-ts");
        let file = dir.path().join("deploy.ts");
        std::fs::write(&file, "console.log('hi')\n").expect("file should be written");
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755))
            .expect("chmod should succeed");
        let meta = std::fs::metadata(&file).expect("metadata should read");

        let (label, command) = build_command(
            &context(vec![PackageManager::Bun]),
            &ResolutionOverrides::default(),
            &file,
            &meta,
            &[String::from("--flag")],
        )
        .expect("an exec-bit source file should build a runtime command");

        assert_eq!(label, "bun", "a shebang-less +x .ts runs via bun, not exec");
        assert_eq!(command.get_program().to_string_lossy(), "bun");
        assert_eq!(
            args_of(&command),
            [file.to_string_lossy().into_owned(), String::from("--flag")],
        );
    }

    #[cfg(unix)]
    #[test]
    fn build_command_execs_exec_bit_source_with_shebang_directly() {
        use std::os::unix::fs::PermissionsExt as _;

        // A `.ts` file with BOTH the exec bit and a `#!` line is a genuine
        // self-executable script: the kernel honors the shebang, so spawn it
        // directly rather than second-guessing the runtime.
        let dir = TempDir::new("local-exec-ts-shebang");
        let file = dir.path().join("deploy.ts");
        std::fs::write(&file, "#!/usr/bin/env -S deno run -A\nconsole.log('hi')\n")
            .expect("file should be written");
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755))
            .expect("chmod should succeed");
        let meta = std::fs::metadata(&file).expect("metadata should read");

        let (label, command) = build_command(
            &context(vec![PackageManager::Bun]),
            &ResolutionOverrides::default(),
            &file,
            &meta,
            &[],
        )
        .expect("an exec-bit shebang file should build a command");

        assert_eq!(label, "exec", "a +x file with a shebang spawns directly");
        assert_eq!(PathBuf::from(command.get_program()), file);
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
    fn try_bare_file_declines_local_prefix_tokens() {
        // Prefix-bearing paths are the pre-task path branch's responsibility;
        // the after-miss fallback must decline them outright. A prefix-less
        // relative path like `bin/tool` is NOT declined here — see
        // `bare_file_resolves_relative_separator_token`.
        let ctx = context(vec![PackageManager::Bun]);
        let defaults = ResolutionOverrides::default();
        for token in ["./main.ts", "../main.ts", "/abs/main.ts", "~/main.ts"] {
            assert!(
                try_bare_file(&ctx, &defaults, token, &[]).is_none(),
                "{token} carries a local prefix and must be declined here",
            );
        }
    }

    #[test]
    fn try_path_token_declines_relative_separator_tokens() {
        // `bin/tool` carries a separator but no explicit local prefix, so the
        // pre-task path branch must decline it (returning `None` without
        // touching the filesystem) — a matching `make bin/tool` task wins
        // first. Only an explicit `./bin/tool` outranks a task.
        let result = try_path_token(
            &context(vec![PackageManager::Bun]),
            &ResolutionOverrides::default(),
            "bin/tool",
            &[],
        )
        .expect("a relative separator token should not error");

        assert!(
            result.is_none(),
            "a prefix-less relative path is left to the after-miss fallback",
        );
    }

    #[test]
    fn bare_file_resolves_relative_separator_token() {
        // The after-miss fallback accepts a prefix-less relative path that
        // carries a separator (`bin/main.ts`), resolving it under the base
        // directory — so an unmatched make-style target name still runs as a
        // local file once task lookup has missed.
        let dir = TempDir::new("bare-file-nested");
        std::fs::create_dir(dir.path().join("bin")).expect("subdir should be created");
        std::fs::write(dir.path().join("bin").join("main.ts"), "console.log(1)\n")
            .expect("file should be written");

        let dispatch = bare_file_in(
            &context(vec![PackageManager::Deno]),
            &ResolutionOverrides::default(),
            dir.path(),
            "bin/main.ts",
            &[],
        )
        .expect("a runnable relative-separator file should dispatch");

        assert_eq!(dispatch.label, "deno run");
        assert_eq!(dispatch.command.get_program().to_string_lossy(), "deno");
    }

    #[test]
    fn try_bare_file_resolves_against_ctx_root() {
        // The bare-file fallback anchors on `ctx.root` (the detected project
        // dir / `--dir` target), not the live process cwd — a `main.ts` under
        // the project root runs even when the shell cwd is elsewhere. This is
        // what stops a `--dir`-set run from missing the file and mis-routing
        // into the `bunx main.ts` 404 fallback (issue #69).
        let dir = TempDir::new("bare-root");
        std::fs::write(dir.path().join("main.ts"), "console.log(1)\n")
            .expect("file should be written");

        let ctx = context_rooted(vec![PackageManager::Deno], dir.path().to_path_buf());
        let dispatch = try_bare_file(&ctx, &ResolutionOverrides::default(), "main.ts", &[])
            .expect("a runnable bare file under ctx.root should dispatch");

        assert_eq!(dispatch.label, "deno run");
        assert_eq!(dispatch.command.get_program().to_string_lossy(), "deno");
    }

    #[test]
    fn try_path_token_resolves_relative_prefix_against_ctx_root() {
        // An explicit `./main.ts` is joined onto `ctx.root`, so it resolves the
        // project-root file regardless of the process cwd — consistent with
        // task detection and the spawned child's working directory.
        let dir = TempDir::new("path-root");
        std::fs::write(dir.path().join("main.ts"), "console.log(1)\n")
            .expect("file should be written");

        let ctx = context_rooted(vec![PackageManager::Bun], dir.path().to_path_buf());
        let dispatch = try_path_token(&ctx, &ResolutionOverrides::default(), "./main.ts", &[])
            .expect("an explicit relative path should not error")
            .expect("a runnable ./main.ts under ctx.root should dispatch");

        assert_eq!(dispatch.label, "bun");
        assert_eq!(dispatch.command.get_program().to_string_lossy(), "bun");
    }
}
