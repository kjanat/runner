//! Go modules, the Go dependency system.

use std::fs;
use std::path::{Path, PathBuf};

use std::process::Command;

/// Directories that may be cleaned in a Go project.
pub(crate) const CLEAN_DIRS: &[&str] = &["vendor"];

/// Detected via `go.mod`.
pub(crate) fn detect(dir: &Path) -> bool {
    find_file(dir).is_some()
}

pub(crate) fn find_file(dir: &Path) -> Option<PathBuf> {
    let path = dir.join("go.mod");
    path.is_file().then_some(path)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExtractedTask {
    pub name: String,
    pub run_target: String,
}

/// Extract local Go commands from root and `cmd/<name>` packages.
pub(crate) fn extract_tasks(dir: &Path) -> anyhow::Result<Vec<ExtractedTask>> {
    let mut tasks = Vec::new();

    if contains_main_package(dir)?
        && let Some(name) = module_name(dir)
            .or_else(|| dir.file_name().and_then(|n| n.to_str()).map(str::to_string))
    {
        tasks.push(ExtractedTask {
            name,
            run_target: ".".to_string(),
        });
    }

    let cmd_dir = dir.join("cmd");
    if !cmd_dir.is_dir() {
        return Ok(tasks);
    }

    for entry in fs::read_dir(cmd_dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() || !contains_main_package(&path)? {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        tasks.push(ExtractedTask {
            name: name.to_string(),
            run_target: format!("./cmd/{name}"),
        });
    }
    tasks.sort_unstable_by(|a, b| a.name.cmp(&b.name));
    Ok(tasks)
}

/// Last path segment of the `module` directive in `dir/go.mod`, used as the
/// root single-binary task name so it tracks the module's identity rather
/// than the filesystem location (cloning to a differently-named directory
/// must not change the task name). A trailing `/vN` major-version suffix is
/// dropped, matching how Go names the produced binary (`example.com/foo/v2`
/// builds `foo`). `None` if `go.mod` is absent or has no parseable `module`
/// line, so the caller can fall back to the directory name.
fn module_name(dir: &Path) -> Option<String> {
    let content = fs::read_to_string(dir.join("go.mod")).ok()?;
    let path = content.lines().find_map(parse_module_line)?;
    let mut segments = path.rsplit('/');
    let last = segments.next()?;
    let name = if is_major_version(last) {
        segments.next().unwrap_or(last)
    } else {
        last
    };
    (!name.is_empty()).then(|| name.to_string())
}

/// Extract the module path from a single `go.mod` line, or `None` if the
/// line is not a `module` directive. Requires a whitespace boundary after
/// `module` so identifiers like `modulefoo` do not match, tolerates a
/// trailing `// comment`, and strips optional surrounding quotes.
fn parse_module_line(line: &str) -> Option<&str> {
    let rest = line.trim_start().strip_prefix("module")?;
    if !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let tok = rest.split_whitespace().next()?.trim_matches('"');
    (!tok.is_empty()).then_some(tok)
}

/// A Go major-version path segment: `v` followed by one or more digits
/// (`v2`, `v10`). These are suffixes on the module path, not the binary
/// name, so they are skipped when deriving the task name.
fn is_major_version(seg: &str) -> bool {
    seg.strip_prefix('v')
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

fn contains_main_package(dir: &Path) -> anyhow::Result<bool> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("go") {
            continue;
        }
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with("_test.go"))
        {
            continue;
        }
        let content = fs::read_to_string(&path)?;
        if content.lines().any(is_main_package_line) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn is_main_package_line(line: &str) -> bool {
    let Some(rest) = line.trim_start().strip_prefix("package") else {
        return false;
    };
    let rest = rest.trim_start();
    let Some(tail) = rest.strip_prefix("main") else {
        return false;
    };
    tail.is_empty()
        || tail.starts_with(char::is_whitespace)
        || tail.starts_with("//")
        || tail.starts_with("/*")
}

/// `go run <target> <args...>`. The caller stamps VCS data once the env
/// layers are applied, through [`stamp_vcs`].
#[cfg(test)]
pub(crate) fn run_cmd(target: &str, args: &[String], _verbosity: super::HostVerbosity) -> Command {
    // `go run` has no quiet flag and no stdout-diversion primitive, so both
    // verbosity axes no-op here.
    let mut c = go_run();
    c.arg(target).args(args);
    c
}

/// `go run`
#[cfg(test)]
fn go_run() -> Command {
    let mut c = super::program::command("go");
    c.arg("run");
    c
}

/// Turn VCS stamping on for a package-form `go run` inside a checkout Go
/// recognises, so `debug.ReadBuildInfo` reports the revision instead of
/// `(devel)`. Go skips the stamp for `go run` by default, and
/// `-buildvcs=true` is a hard error outside a checkout, with the VCS tool
/// missing, or on a toolchain before 1.18, so each is checked first. A
/// `GOFLAGS` the command already carries, from the env layers, is what gets
/// merged; the process's own is the fallback.
pub(crate) fn stamp_vcs(command: &mut Command, project: &Path) {
    let Some(tool) = go_vcs_tool(project) else {
        return;
    };
    if !tool_on_path(tool) || !toolchain_stamps_vcs() {
        return;
    }
    if let Some(flags) = goflags_with_buildvcs(command_goflags(command).as_deref()) {
        command.env("GOFLAGS", flags);
    }
}

/// The `GOFLAGS` the child will see: the command's own entry when one is
/// set, nothing when it was removed, else the inherited one.
fn command_goflags(command: &Command) -> Option<String> {
    match command.get_envs().find(|(key, _)| *key == "GOFLAGS") {
        Some((_, Some(value))) => Some(value.to_string_lossy().into_owned()),
        Some((_, None)) => None,
        None => std::env::var("GOFLAGS").ok(),
    }
}

/// The version-control tool Go would invoke for `project`, from the nearest
/// checkout marker above it. Go's `cmd/go/internal/vcs` knows Git,
/// Mercurial, Subversion, Bazaar and Fossil; a native Jujutsu checkout has
/// no marker Go reads.
fn go_vcs_tool(project: &Path) -> Option<&'static str> {
    const MARKERS: [(&str, &str); 6] = [
        (".git", "git"),
        (".hg", "hg"),
        (".svn", "svn"),
        (".bzr", "bzr"),
        (".fslckout", "fossil"),
        ("_FOSSIL_", "fossil"),
    ];
    project.ancestors().find_map(|dir| {
        MARKERS
            .iter()
            .find(|(marker, _)| dir.join(marker).exists())
            .map(|(_, tool)| *tool)
    })
}

fn tool_on_path(tool: &str) -> bool {
    let path = std::env::var_os("PATH").unwrap_or_default();
    crate::resolver::probe::probe_in(tool, &path, std::env::var_os("PATHEXT").as_deref()).is_some()
}

/// Whether the `go` on `PATH` knows `-buildvcs`, which arrived in Go 1.18.
/// A missing `go` returns `true` so the spawn reports the absence itself.
fn toolchain_stamps_vcs() -> bool {
    match super::program::command("go").arg("version").output() {
        Ok(output) => supports_buildvcs(&String::from_utf8_lossy(&output.stdout)),
        Err(_) => true,
    }
}

/// Read a `go version` line. A line naming no `go1.N` release, such as a
/// devel build, counts as new enough.
fn supports_buildvcs(version_line: &str) -> bool {
    let Some(rest) = version_line
        .split_whitespace()
        .find_map(|word| word.strip_prefix("go1."))
    else {
        return true;
    };
    rest.split(|c: char| !c.is_ascii_digit())
        .next()
        .and_then(|digits| digits.parse::<u32>().ok())
        .is_none_or(|minor| minor >= 18)
}

/// `GOFLAGS` with `-buildvcs=true` appended, or `None` when the caller
/// already decided `-buildvcs` either way, in the `-` or `--` spelling.
fn goflags_with_buildvcs(existing: Option<&str>) -> Option<String> {
    let existing = existing.unwrap_or("").trim();
    if existing.split_whitespace().any(decides_buildvcs) {
        return None;
    }
    Some(if existing.is_empty() {
        "-buildvcs=true".to_string()
    } else {
        format!("{existing} -buildvcs=true")
    })
}

fn decides_buildvcs(flag: &str) -> bool {
    let name = flag
        .strip_prefix("--")
        .or_else(|| flag.strip_prefix('-'))
        .unwrap_or("");
    name == "buildvcs" || name.starts_with("buildvcs=")
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{ExtractedTask, extract_tasks, run_cmd};

    use crate::tool::test_support::TempDir;

    fn task(name: &str, run_target: &str) -> ExtractedTask {
        ExtractedTask {
            name: name.to_string(),
            run_target: run_target.to_string(),
        }
    }

    #[test]
    fn extract_tasks_finds_cmd_main_packages() {
        let dir = TempDir::new("go-cmd-tasks");
        fs::write(dir.path().join("go.mod"), "module example.com/app\n")
            .expect("go.mod should be written");
        fs::create_dir_all(dir.path().join("cmd").join("serve"))
            .expect("serve dir should be created");
        fs::create_dir_all(dir.path().join("cmd").join("internal-lib"))
            .expect("internal-lib dir should be created");
        fs::write(
            dir.path().join("cmd").join("serve").join("main.go"),
            "package main\n\nfunc main() {}\n",
        )
        .expect("serve main should be written");
        fs::write(
            dir.path().join("cmd").join("internal-lib").join("lib.go"),
            "package lib\n",
        )
        .expect("lib source should be written");

        let tasks = extract_tasks(dir.path()).expect("go cmd tasks should parse");

        assert_eq!(tasks, [task("serve", "./cmd/serve")]);
    }

    #[test]
    fn extract_tasks_root_name_from_go_mod_module() {
        let dir = TempDir::new("go-root-main");
        fs::write(
            dir.path().join("go.mod"),
            "module github.com/kjanat/some-cli-app // root\n",
        )
        .expect("go.mod should be written");
        fs::write(
            dir.path().join("main.go"),
            "package main\n\nfunc main() {}\n",
        )
        .expect("root main should be written");

        // Last module-path segment, not the (temp, randomized) directory name.
        let tasks = extract_tasks(dir.path()).expect("go root task should parse");

        assert_eq!(tasks, [task("some-cli-app", ".")]);
    }

    #[test]
    fn extract_tasks_root_name_drops_major_version_suffix() {
        let dir = TempDir::new("go-root-v2");
        fs::write(dir.path().join("go.mod"), "module example.com/widget/v2\n")
            .expect("go.mod should be written");
        fs::write(
            dir.path().join("main.go"),
            "package main\n\nfunc main() {}\n",
        )
        .expect("root main should be written");

        let tasks = extract_tasks(dir.path()).expect("go root task should parse");

        assert_eq!(tasks, [task("widget", ".")]);
    }

    #[test]
    fn extract_tasks_root_name_falls_back_to_dir_without_module_line() {
        let dir = TempDir::new("go-root-no-module");
        // go.mod present (project still detected) but no `module` directive.
        fs::write(dir.path().join("go.mod"), "go 1.22\n").expect("go.mod should be written");
        fs::write(
            dir.path().join("main.go"),
            "package main\n\nfunc main() {}\n",
        )
        .expect("root main should be written");

        let tasks = extract_tasks(dir.path()).expect("go root task should parse");
        let name = dir
            .path()
            .file_name()
            .and_then(|name| name.to_str())
            .expect("temp dir should have utf-8 file name");

        assert_eq!(tasks, [task(name, ".")]);
    }

    #[test]
    fn extract_tasks_ignores_main_test_packages() {
        let dir = TempDir::new("go-cmd-test-only");
        fs::write(dir.path().join("go.mod"), "module example.com/app\n")
            .expect("go.mod should be written");
        fs::create_dir_all(dir.path().join("cmd").join("serve"))
            .expect("serve dir should be created");
        fs::write(
            dir.path().join("cmd").join("serve").join("main_test.go"),
            "package main\n\nfunc TestServe() {}\n",
        )
        .expect("test main should be written");

        let tasks = extract_tasks(dir.path()).expect("go cmd tasks should parse");

        assert!(tasks.is_empty());
    }

    #[test]
    fn extract_tasks_accepts_commented_main_package_clause() {
        let dir = TempDir::new("go-cmd-main-comment");
        fs::write(dir.path().join("go.mod"), "module example.com/app\n")
            .expect("go.mod should be written");
        fs::create_dir_all(dir.path().join("cmd").join("serve"))
            .expect("serve dir should be created");
        fs::write(
            dir.path().join("cmd").join("serve").join("main.go"),
            "package main // command\n\nfunc main() {}\n",
        )
        .expect("main should be written");

        let tasks = extract_tasks(dir.path()).expect("go cmd tasks should parse");

        assert_eq!(tasks, [task("serve", "./cmd/serve")]);
    }

    #[test]
    fn run_cmd_uses_go_run_target() {
        let args = [String::from("--port"), String::from("3000")];
        let built: Vec<_> = run_cmd("./cmd/serve", &args, crate::tool::HostVerbosity::default())
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        assert_eq!(built, ["run", "./cmd/serve", "--port", "3000"]);
    }

    #[test]
    fn goflags_gain_buildvcs_unless_already_decided() {
        use super::goflags_with_buildvcs;
        assert_eq!(
            goflags_with_buildvcs(None).as_deref(),
            Some("-buildvcs=true")
        );
        assert_eq!(
            goflags_with_buildvcs(Some("")).as_deref(),
            Some("-buildvcs=true")
        );
        assert_eq!(
            goflags_with_buildvcs(Some("-mod=vendor")).as_deref(),
            Some("-mod=vendor -buildvcs=true")
        );
        assert_eq!(goflags_with_buildvcs(Some("-buildvcs=false")), None);
        assert_eq!(goflags_with_buildvcs(Some("--buildvcs=false")), None);
        assert_eq!(goflags_with_buildvcs(Some("-buildvcs")), None);
        assert_eq!(
            goflags_with_buildvcs(Some("-mod=vendor -buildvcs=auto")),
            None
        );
        assert_eq!(
            goflags_with_buildvcs(Some("-tags=buildvcs")).as_deref(),
            Some("-tags=buildvcs -buildvcs=true")
        );
    }

    #[test]
    fn go_version_lines_gate_buildvcs_at_1_18() {
        use super::supports_buildvcs;
        assert!(supports_buildvcs(
            "go version go1.27.1-X:nodwarf5 linux/amd64"
        ));
        assert!(supports_buildvcs("go version go1.18 linux/amd64"));
        assert!(!supports_buildvcs("go version go1.17.13 linux/amd64"));
        assert!(supports_buildvcs("go version devel +abc123 linux/amd64"));
    }

    #[test]
    fn go_vcs_tool_knows_what_go_knows() {
        use super::go_vcs_tool;
        let dir = TempDir::new("go-vcs-tool");
        let project = dir.path().join("svc");
        fs::create_dir_all(&project).expect("project dir");
        assert_eq!(go_vcs_tool(&project), None);

        fs::create_dir_all(dir.path().join(".jj")).expect("jj dir");
        assert_eq!(
            go_vcs_tool(&project),
            None,
            "Go does not read a jj checkout"
        );

        fs::create_dir_all(dir.path().join(".hg")).expect("hg dir");
        assert_eq!(go_vcs_tool(&project), Some("hg"));

        fs::write(dir.path().join(".git"), "gitdir: elsewhere\n").expect("git file");
        assert_eq!(go_vcs_tool(&project), Some("git"));
    }

    fn goflags_of(command: &std::process::Command) -> Option<String> {
        command
            .get_envs()
            .find(|(k, _)| *k == "GOFLAGS")
            .and_then(|(_, v)| v)
            .map(|v| v.to_string_lossy().into_owned())
    }

    #[test]
    fn stamp_vcs_merges_after_env_layers_and_only_inside_a_git_checkout() {
        use super::stamp_vcs;
        if crate::resolver::probe::probe_in(
            "git",
            &std::env::var_os("PATH").unwrap_or_default(),
            std::env::var_os("PATHEXT").as_deref(),
        )
        .is_none()
        {
            eprintln!("skipping: `git` not found on PATH");
            return;
        }
        let dir = TempDir::new("go-buildvcs");

        let mut outside = run_cmd("./cmd/svc", &[], crate::tool::HostVerbosity::default());
        stamp_vcs(&mut outside, dir.path());
        assert_eq!(goflags_of(&outside), None);

        fs::create_dir_all(dir.path().join(".git")).expect("git dir should be created");
        let mut inside = run_cmd("./cmd/svc", &[], crate::tool::HostVerbosity::default());
        stamp_vcs(&mut inside, dir.path());
        assert!(
            goflags_of(&inside).is_some_and(|v| v.ends_with("-buildvcs=true")),
            "GOFLAGS should carry -buildvcs=true inside a checkout"
        );

        let mut layered = run_cmd("./cmd/svc", &[], crate::tool::HostVerbosity::default());
        layered.env("GOFLAGS", "-mod=vendor");
        stamp_vcs(&mut layered, dir.path());
        assert_eq!(
            goflags_of(&layered).as_deref(),
            Some("-mod=vendor -buildvcs=true")
        );

        let mut decided = run_cmd("./cmd/svc", &[], crate::tool::HostVerbosity::default());
        decided.env("GOFLAGS", "--buildvcs=false");
        stamp_vcs(&mut decided, dir.path());
        assert_eq!(goflags_of(&decided).as_deref(), Some("--buildvcs=false"));
    }
}
