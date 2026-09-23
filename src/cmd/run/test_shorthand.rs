//! `run test` without a `test` task: each ecosystem's built-in test runner.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Result, bail};

use crate::resolver::ResolutionOverrides;
use crate::tool;
use crate::types::{JsRuntime, PackageManager, ProjectContext, TaskSource};

const EXTENSIONS: &[&str] = &["js", "mjs", "cjs", "jsx", "ts", "mts", "cts", "tsx"];

const SKIPPED_DIRS: &[&str] = &[
    "node_modules",
    ".git",
    "dist",
    "build",
    "out",
    "coverage",
    "target",
    "vendor",
    ".next",
    ".nuxt",
    ".svelte-kit",
    ".turbo",
];

/// The runner for `run test` when no task source declares `test`, or `None`
/// when no detected ecosystem ships one.
pub(super) fn resolve(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    resolved_pm: Option<PackageManager>,
    task: &str,
    args: &[String],
) -> Result<Option<(&'static str, Command)>> {
    if task != "test" || has_package_script(ctx, task) {
        return Ok(None);
    }
    if let Some(runtime) = overrides.js_runtime() {
        return js_runtime(runtime, &ctx.cwd, args).map(Some);
    }
    if let Some(pm) = resolved_pm {
        return package_manager(ctx, overrides, pm, args);
    }
    for pm in ctx.package_managers.iter().copied() {
        if let Some(found) = package_manager(ctx, overrides, pm, args)? {
            return Ok(Some(found));
        }
    }
    Ok(None)
}

fn package_manager(
    ctx: &ProjectContext,
    overrides: &ResolutionOverrides,
    pm: PackageManager,
    args: &[String],
) -> Result<Option<(&'static str, Command)>> {
    let found = match pm {
        PackageManager::Bun => js_runtime(JsRuntime::Bun, &ctx.cwd, args)?,
        PackageManager::Deno => js_runtime(JsRuntime::Deno, &ctx.cwd, args)?,
        PackageManager::Npm | PackageManager::Yarn | PackageManager::Pnpm => {
            js_runtime(JsRuntime::Node, &ctx.cwd, args)?
        }
        PackageManager::Cargo => ("cargo", subcommand("cargo", &["test"], args)),
        PackageManager::Go => ("go", subcommand("go", &["test", "./..."], args)),
        PackageManager::Uv | PackageManager::Poetry | PackageManager::Pipenv => {
            let Some(python) = super::dispatch::resolve_python_pm(ctx, overrides) else {
                return Ok(None);
            };
            let mut c = tool::program::command(python.pm.label());
            c.arg("run").args(python_runner(ctx).argv()).args(args);
            (python.pm.label(), c)
        }
        PackageManager::Bundler | PackageManager::Composer => return Ok(None),
    };
    Ok(Some(found))
}

/// The Python test runner `run test` picks, most specific first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PythonRunner {
    Pytest,
    Nose2,
    Ward,
    Django,
    Tox,
    Nox,
    Unittest,
}

impl PythonRunner {
    /// What follows `<pm> run`.
    pub(super) const fn argv(self) -> &'static [&'static str] {
        match self {
            Self::Pytest => &["pytest"],
            Self::Nose2 => &["nose2"],
            Self::Ward => &["ward"],
            Self::Django => &["python", "manage.py", "test"],
            Self::Tox => &["tox"],
            Self::Nox => &["nox"],
            Self::Unittest => &["python", "-m", "unittest"],
        }
    }
}

/// pytest, nose2 and ward count when installed in the project's virtualenv,
/// pytest also when its config is present since `uv run` syncs it in. Django
/// needs a `manage.py`, tox and nox their config plus the tool on the venv or
/// `PATH`. `unittest` ships with Python and is the floor.
pub(super) fn python_runner(ctx: &ProjectContext) -> PythonRunner {
    let dirs = [ctx.cwd.as_path(), ctx.root.as_path()];
    let venvs = venvs(&dirs);
    let in_venv = |name: &str| venvs.iter().any(|env| venv_has(env, name));
    let file = |name: &str| dirs.iter().any(|dir| dir.join(name).is_file());
    let on_path = |name: &str| {
        std::env::var_os("PATH").is_some_and(|path| {
            crate::resolver::probe::probe_in(name, &path, std::env::var_os("PATHEXT").as_deref())
                .is_some()
        })
    };
    if in_venv("pytest") || file("pytest.ini") || file("conftest.py") {
        PythonRunner::Pytest
    } else if in_venv("nose2") {
        PythonRunner::Nose2
    } else if in_venv("ward") {
        PythonRunner::Ward
    } else if file("manage.py") {
        PythonRunner::Django
    } else if file("tox.ini") && (in_venv("tox") || on_path("tox")) {
        PythonRunner::Tox
    } else if file("noxfile.py") && (in_venv("nox") || on_path("nox")) {
        PythonRunner::Nox
    } else {
        PythonRunner::Unittest
    }
}

fn venvs(dirs: &[&Path]) -> Vec<PathBuf> {
    let mut envs: Vec<PathBuf> = dirs.iter().map(|dir| dir.join(".venv")).collect();
    if let Some(active) = std::env::var_os("VIRTUAL_ENV") {
        envs.push(PathBuf::from(active));
    }
    envs
}

fn venv_has(env: &Path, name: &str) -> bool {
    env.join("bin").join(name).is_file()
        || env.join("Scripts").join(format!("{name}.exe")).is_file()
}

fn js_runtime(runtime: JsRuntime, cwd: &Path, args: &[String]) -> Result<(&'static str, Command)> {
    Ok(match runtime {
        JsRuntime::Bun => ("bun", tool::bun::test_cmd(args)),
        JsRuntime::Deno => ("deno", subcommand("deno", &["test"], args)),
        JsRuntime::Node => ("node", node_test_cmd(cwd, args)?),
    })
}

fn subcommand(program: &str, fixed: &[&str], args: &[String]) -> Command {
    let mut c = tool::program::command(program);
    c.args(fixed).args(args);
    c
}

/// `node --test` over the files `run test` discovers, unless `args` already
/// names targets of its own.
fn node_test_cmd(cwd: &Path, args: &[String]) -> Result<Command> {
    let mut c = tool::program::command("node");
    let user_targets = args.iter().any(|arg| !arg.starts_with('-'));
    let files = if user_targets {
        Vec::new()
    } else {
        discover_test_files(cwd)
    };
    if !user_targets && files.is_empty() {
        bail!(
            "no test files found under {}: expected test.<ext> or *.test.<ext> with one of {}",
            cwd.display(),
            EXTENSIONS.join(", ")
        );
    }
    if files.iter().any(|file| is_typescript(file)) {
        c.arg("--experimental-strip-types");
    }
    c.arg("--test").args(args).args(&files);
    Ok(c)
}

fn is_typescript(file: &Path) -> bool {
    file.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| matches!(ext, "ts" | "mts" | "cts" | "tsx"))
}

/// Every `test.<ext>` and `*.test.<ext>` below `root`, sorted, relative to
/// it, skipping dependency and build output directories.
pub(super) fn discover_test_files(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    walk(root, root, &mut found);
    found.sort();
    found
}

fn walk(root: &Path, dir: &Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if path.is_dir() {
            if !SKIPPED_DIRS.contains(&name) {
                walk(root, &path, found);
            }
        } else if is_test_file(name) {
            let relative = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
            found.push(relative);
        }
    }
}

fn is_test_file(name: &str) -> bool {
    let Some((stem, ext)) = name.rsplit_once('.') else {
        return false;
    };
    let last = stem.rsplit_once('.').map_or(stem, |(_, last)| last);
    EXTENSIONS.contains(&ext) && last == "test"
}

fn has_package_script(ctx: &ProjectContext, task: &str) -> bool {
    ctx.tasks.iter().any(|entry| {
        entry.source == TaskSource::PackageJson && entry.member.is_none() && entry.name == task
    })
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::{discover_test_files, is_test_file, resolve};
    use crate::resolver::ResolutionOverrides;
    use crate::tool::test_support::TempDir;
    use crate::types::{PackageManager, ProjectContext, Task, TaskSource};

    fn context(package_managers: Vec<PackageManager>, tasks: Vec<Task>) -> ProjectContext {
        ProjectContext {
            cwd: PathBuf::from("."),
            root: PathBuf::from("."),
            package_managers,
            task_runners: Vec::new(),
            tasks,
            node_version: None,
            current_node: None,
            is_monorepo: false,
            workspace: None,
            install_dirs: Vec::new(),
            warnings: Vec::new(),
        }
    }

    fn args(command: &std::process::Command) -> Vec<String> {
        command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    fn label(
        ctx: &ProjectContext,
        resolved_pm: Option<PackageManager>,
        task: &str,
    ) -> Option<&'static str> {
        resolve(ctx, &ResolutionOverrides::default(), resolved_pm, task, &[])
            .expect("resolves")
            .map(|(label, _)| label)
    }

    #[test]
    fn matches_only_test_dot_ext_and_dot_test_dot_ext() {
        assert!(is_test_file("test.ts"));
        assert!(is_test_file("foo.test.mjs"));
        assert!(is_test_file("a.b.test.tsx"));
        assert!(!is_test_file("test.rs"));
        assert!(!is_test_file("foo.spec.ts"));
        assert!(!is_test_file("testing.ts"));
        assert!(!is_test_file("foo_test.js"));
    }

    #[test]
    fn discovery_skips_dependency_and_output_dirs() {
        let dir = TempDir::new("test-discovery");
        for rel in [
            "src/a.test.ts",
            "src/deep/test.js",
            "node_modules/pkg/x.test.js",
            "dist/y.test.js",
            "src/main.ts",
        ] {
            let path = dir.path().join(rel);
            fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
            fs::write(&path, "").expect("file");
        }

        let found = discover_test_files(dir.path());
        assert_eq!(
            found,
            [
                PathBuf::from("src/a.test.ts"),
                PathBuf::from("src/deep/test.js")
            ]
        );
    }

    #[test]
    fn node_family_runs_node_test_over_discovered_files() {
        let dir = TempDir::new("node-test");
        fs::write(dir.path().join("test.ts"), "").expect("file");
        let mut ctx = context(vec![PackageManager::Npm], vec![]);
        ctx.cwd = dir.path().to_path_buf();

        let (label, command) = resolve(
            &ctx,
            &ResolutionOverrides::default(),
            Some(PackageManager::Npm),
            "test",
            &[],
        )
        .expect("resolves")
        .expect("shorthand");
        assert_eq!(label, "node");
        assert_eq!(
            args(&command),
            ["--experimental-strip-types", "--test", "test.ts"]
        );
    }

    #[test]
    fn node_family_without_test_files_is_an_error() {
        let dir = TempDir::new("node-test-empty");
        let mut ctx = context(vec![PackageManager::Pnpm], vec![]);
        ctx.cwd = dir.path().to_path_buf();

        let err = resolve(
            &ctx,
            &ResolutionOverrides::default(),
            Some(PackageManager::Pnpm),
            "test",
            &[],
        )
        .expect_err("no files");
        assert!(format!("{err:#}").contains("no test files found"));
    }

    #[test]
    fn user_targets_skip_discovery() {
        let dir = TempDir::new("node-test-targets");
        let mut ctx = context(vec![PackageManager::Yarn], vec![]);
        ctx.cwd = dir.path().to_path_buf();

        let (_, command) = resolve(
            &ctx,
            &ResolutionOverrides::default(),
            Some(PackageManager::Yarn),
            "test",
            &[String::from("src/only.test.js")],
        )
        .expect("resolves")
        .expect("shorthand");
        assert_eq!(args(&command), ["--test", "src/only.test.js"]);
    }

    #[test]
    fn bun_deno_cargo_go_and_python_have_builtin_runners() {
        assert_eq!(
            label(&context(vec![], vec![]), Some(PackageManager::Bun), "test"),
            Some("bun")
        );
        assert_eq!(
            label(&context(vec![], vec![]), Some(PackageManager::Deno), "test"),
            Some("deno")
        );
        let (_, cargo) = resolve(
            &context(vec![PackageManager::Cargo], vec![]),
            &ResolutionOverrides::default(),
            None,
            "test",
            &[String::from("--release")],
        )
        .expect("resolves")
        .expect("shorthand");
        assert_eq!(args(&cargo), ["test", "--release"]);
        let (_, go) = resolve(
            &context(vec![PackageManager::Go], vec![]),
            &ResolutionOverrides::default(),
            None,
            "test",
            &[],
        )
        .expect("resolves")
        .expect("shorthand");
        assert_eq!(args(&go), ["test", "./..."]);
        let (label, uv) = resolve(
            &context(vec![PackageManager::Uv], vec![]),
            &ResolutionOverrides::default(),
            None,
            "test",
            &[],
        )
        .expect("resolves")
        .expect("shorthand");
        assert_eq!(label, "uv");
        assert_eq!(args(&uv), ["run", "python", "-m", "unittest"]);
    }

    #[test]
    fn python_prefers_pytest_when_the_venv_has_it() {
        let dir = TempDir::new("pytest-venv");
        let bin = dir.path().join(".venv").join("bin");
        fs::create_dir_all(&bin).expect("bin");
        fs::write(bin.join("pytest"), "").expect("pytest");
        let mut ctx = context(vec![PackageManager::Uv], vec![]);
        ctx.cwd = dir.path().to_path_buf();
        ctx.root = dir.path().to_path_buf();

        let (_, command) = resolve(
            &ctx,
            &ResolutionOverrides::default(),
            None,
            "test",
            &[String::from("-k"), String::from("smoke")],
        )
        .expect("resolves")
        .expect("shorthand");
        assert_eq!(args(&command), ["run", "pytest", "-k", "smoke"]);
    }

    #[test]
    fn python_runner_detection_order() {
        use super::{PythonRunner, python_runner};
        let dir = TempDir::new("python-runners");
        let mut ctx = context(vec![PackageManager::Poetry], vec![]);
        ctx.cwd = dir.path().to_path_buf();
        ctx.root = dir.path().to_path_buf();
        let bin = dir.path().join(".venv").join("bin");
        fs::create_dir_all(&bin).expect("bin");

        assert_eq!(python_runner(&ctx), PythonRunner::Unittest);
        fs::write(dir.path().join("manage.py"), "").expect("manage");
        assert_eq!(python_runner(&ctx), PythonRunner::Django);
        fs::write(bin.join("ward"), "").expect("ward");
        assert_eq!(python_runner(&ctx), PythonRunner::Ward);
        fs::write(bin.join("nose2"), "").expect("nose2");
        assert_eq!(python_runner(&ctx), PythonRunner::Nose2);
        fs::write(dir.path().join("conftest.py"), "").expect("conftest");
        assert_eq!(python_runner(&ctx), PythonRunner::Pytest);
    }

    #[test]
    fn tox_and_nox_need_their_config_and_the_tool() {
        use super::{PythonRunner, python_runner};
        let dir = TempDir::new("python-tox");
        let mut ctx = context(vec![PackageManager::Uv], vec![]);
        ctx.cwd = dir.path().to_path_buf();
        ctx.root = dir.path().to_path_buf();
        let bin = dir.path().join(".venv").join("bin");
        fs::create_dir_all(&bin).expect("bin");
        fs::write(bin.join("tox"), "").expect("tox");
        fs::write(bin.join("nox"), "").expect("nox");

        assert_eq!(python_runner(&ctx), PythonRunner::Unittest);
        fs::write(dir.path().join("noxfile.py"), "").expect("noxfile");
        assert_eq!(python_runner(&ctx), PythonRunner::Nox);
        fs::write(dir.path().join("tox.ini"), "").expect("tox.ini");
        assert_eq!(python_runner(&ctx), PythonRunner::Tox);
    }

    #[test]
    fn ruby_and_php_have_none() {
        assert_eq!(
            label(
                &context(vec![PackageManager::Bundler], vec![]),
                None,
                "test"
            ),
            None
        );
        assert_eq!(
            label(
                &context(vec![PackageManager::Composer], vec![]),
                None,
                "test"
            ),
            None
        );
    }

    #[test]
    fn disabled_when_a_test_script_exists_or_the_token_differs() {
        let with_script = context(
            vec![PackageManager::Bun],
            vec![Task {
                name: "test".to_string(),
                source: TaskSource::PackageJson,
                run_target: None,
                description: None,
                alias_of: None,
                passthrough_to: None,
                detail: crate::types::TaskDetail::default(),
                member: None,
            }],
        );
        assert_eq!(label(&with_script, Some(PackageManager::Bun), "test"), None);
        assert_eq!(
            label(
                &context(vec![PackageManager::Bun], vec![]),
                Some(PackageManager::Bun),
                "build"
            ),
            None
        );
    }

    #[test]
    fn resolved_pm_outranks_detection() {
        let ctx = context(vec![PackageManager::Bun], vec![]);
        assert_eq!(label(&ctx, Some(PackageManager::Bun), "test"), Some("bun"));
    }
}
