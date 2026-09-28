//! Go modules, the Go dependency system.

use std::fs;
use std::path::{Path, PathBuf};

/// Detected via `go.mod`.
#[must_use]
pub fn detect(dir: &Path) -> bool {
    find_file(dir).is_some()
}

/// The module manifest in this directory.
#[must_use]
pub fn find_file(dir: &Path) -> Option<PathBuf> {
    let path = dir.join("go.mod");
    path.is_file().then_some(path)
}

/// A runnable Go package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedTask {
    /// The exposed task name.
    pub name: String,
    /// The package path passed to Go.
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

/// A `vcs` variant for each scope Go runs tasks in inside a checkout, so a
/// `go run` there stamps the revision instead of `(devel)`.
///
/// Go skips the stamp for `go run` by default, and `-buildvcs=true` is a hard
/// error outside a checkout, with the VCS tool missing, or on a toolchain
/// before 1.18, so each is checked first. A `GOFLAGS` that already decides
/// `-buildvcs` either way leaves the choice to it.
///
/// # Errors
/// Never; the signature is the observation hook's.
pub fn vcs_variant(
    tree: &runner_core::Tree,
    evidence: &[runner_core::Evidence],
) -> std::io::Result<Vec<runner_core::Evidence>> {
    let goflags = std::env::var("GOFLAGS").ok();
    if goflags
        .as_deref()
        .is_some_and(|flags| flags.split_whitespace().any(decides_buildvcs))
    {
        return Ok(Vec::new());
    }
    Ok(checkout_variants(tree, evidence, &toolchain_stamps_vcs))
}

fn checkout_variants(
    tree: &runner_core::Tree,
    evidence: &[runner_core::Evidence],
    stamps: &dyn Fn() -> bool,
) -> Vec<runner_core::Evidence> {
    let mut derived: Vec<runner_core::Evidence> = Vec::new();
    let mut toolchain = None;
    for item in evidence.iter().filter(|item| {
        item.provider == Some(runner_core::ProviderId::Go)
            && item.weight <= runner_core::Weight::Configured
    }) {
        if derived.iter().any(|seen| seen.scope == item.scope) {
            continue;
        }
        let dir = runner_core::scope_dir(tree, &item.scope);
        let Some((marker, tool)) = go_vcs_tool(&dir) else {
            continue;
        };
        if !tool_on_path(tool) || !*toolchain.get_or_insert_with(stamps) {
            continue;
        }
        derived.push(runner_core::Evidence {
            provider: Some(runner_core::ProviderId::Go),
            signal: None,
            at: marker,
            scope: item.scope.clone(),
            weight: runner_core::Weight::Probed,
            declared: Some(runner_core::Declared::Variant("vcs".into())),
        });
    }
    derived
}

/// The checkout marker nearest above `project` and the version-control tool
/// Go would invoke for it. Go's `cmd/go/internal/vcs` knows Git,
/// Mercurial, Subversion, Bazaar and Fossil; a native Jujutsu checkout has
/// no marker Go reads.
fn go_vcs_tool(project: &Path) -> Option<(PathBuf, &'static str)> {
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
            .map(|(marker, tool)| (dir.join(marker), *tool))
            .find(|(marker, _)| marker.exists())
    })
}

fn tool_on_path(tool: &str) -> bool {
    let path = std::env::var_os("PATH").unwrap_or_default();
    runner_core::probe::probe_in(tool, &path, std::env::var_os("PATHEXT").as_deref()).is_some()
}

/// Whether the `go` on `PATH` knows `-buildvcs`, which arrived in Go 1.18.
/// A missing `go` returns `true` so the spawn reports the absence itself.
fn toolchain_stamps_vcs() -> bool {
    match super::command("go").arg("version").output() {
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

fn decides_buildvcs(flag: &str) -> bool {
    let name = flag
        .strip_prefix("--")
        .or_else(|| flag.strip_prefix('-'))
        .unwrap_or("");
    name == "buildvcs" || name.starts_with("buildvcs=")
}

/// Tasks available in the observed project scope.
///
/// # Errors
/// Reports errors reading task sources.
pub fn tasks(
    present: &runner_core::Present,
    tree: &runner_core::Tree,
) -> Result<runner_core::Extracted, runner_core::Warning> {
    let root = runner_core::plan::scope_dir(tree, &present.scope);
    let extracted = extract_tasks(&root)
        .map_err(|e| runner_core::Warning::about(present.provider, format!("{e:#}")))?;
    Ok(extracted
        .into_iter()
        .map(|entry| {
            let mut task = super::task(present, entry.name, None);
            task.target = Some(entry.run_target);
            task
        })
        .collect::<Vec<_>>()
        .into())
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::test_support::TempDir;
    use std::fs;
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

        assert_eq!(tasks.len(), 0);
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
    fn a_goflags_flag_decides_buildvcs_in_either_spelling() {
        use super::decides_buildvcs;
        assert!(decides_buildvcs("-buildvcs=false"));
        assert!(decides_buildvcs("--buildvcs=false"));
        assert!(decides_buildvcs("-buildvcs"));
        assert!(decides_buildvcs("-buildvcs=auto"));
        assert!(!decides_buildvcs("-tags=buildvcs"));
        assert!(!decides_buildvcs("-mod=vendor"));
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
        assert_eq!(go_vcs_tool(&project), Some((dir.path().join(".hg"), "hg")));

        fs::write(dir.path().join(".git"), "gitdir: elsewhere\n").expect("git file");
        assert_eq!(
            go_vcs_tool(&project),
            Some((dir.path().join(".git"), "git"))
        );
    }

    #[test]
    fn a_go_scope_inside_a_git_checkout_gets_the_vcs_variant() {
        use super::checkout_variants;
        if runner_core::probe::probe_in(
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
        let tree = runner_core::Tree {
            cwd: dir.path().to_owned(),
            root: dir.path().to_owned(),
            members: Vec::new(),
        };
        let go_mod = runner_core::Evidence {
            provider: Some(runner_core::ProviderId::Go),
            signal: Some(runner_core::SignalId(0)),
            at: dir.path().join("go.mod"),
            scope: runner_core::Scope::Root,
            weight: runner_core::Weight::Configured,
            declared: None,
        };

        assert_eq!(
            checkout_variants(&tree, std::slice::from_ref(&go_mod), &|| true),
            []
        );

        fs::create_dir_all(dir.path().join(".git")).expect("git dir should be created");
        let derived = checkout_variants(&tree, std::slice::from_ref(&go_mod), &|| true);
        assert_eq!(derived.len(), 1);
        assert_eq!(
            derived[0].declared,
            Some(runner_core::Declared::Variant("vcs".into()))
        );
        assert!(
            checkout_variants(&tree, std::slice::from_ref(&go_mod), &|| false).is_empty(),
            "a toolchain before 1.18 rejects -buildvcs"
        );
    }
}
