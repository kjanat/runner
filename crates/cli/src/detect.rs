//! Project detection: scans the working directory for config/lock files and
//! builds a [`ProjectContext`] describing the detected toolchain.

use std::path::Path;
use std::sync::Arc;

use crate::provider::Named;
use crate::tool;
use crate::types::{DetectionWarning, ProjectContext, Task, Workspace, WorkspaceMember};

/// The workspace or project root a directory is anchored in.
pub(crate) struct Anchored {
    workspace: Option<Workspace>,
    warning: Option<runner_core::Warning>,
    pub root: std::path::PathBuf,
}

/// Anchor `dir` in its workspace or project root.
pub(crate) fn anchored(dir: &Path) -> Anchored {
    anchored_below(dir, crate::home_dir().as_deref())
}

/// Anchor `dir` in its workspace or project root, searching no higher than
/// its repository or, outside one, than the directory below `home` or `/`.
fn anchored_below(dir: &Path, home: Option<&Path>) -> Anchored {
    let boundary = boundary(dir, home);
    let (workspace, warning) = anchor(dir, &boundary);
    let root = workspace.as_ref().map_or_else(
        || project_root(dir, &boundary),
        |workspace| workspace.root.clone(),
    );
    Anchored {
        workspace,
        warning,
        root,
    }
}

/// Anchor `dir` in its workspace or project root, then observe and resolve
/// that tree under `overrides`.
pub(crate) fn detect(
    dir: &Path,
    overrides: &crate::resolver::ResolutionOverrides,
) -> ProjectContext {
    detect_anchored(dir, anchored(dir), overrides)
}

/// Observe and resolve the tree `anchored` roots, invoked from `dir`, under
/// `overrides`.
pub(crate) fn detect_anchored(
    dir: &Path,
    anchored: Anchored,
    overrides: &crate::resolver::ResolutionOverrides,
) -> ProjectContext {
    let mut ctx = ProjectContext {
        cwd: dir.to_path_buf(),
        root: anchored.root,
        tasks: Vec::new(),
        workspace: anchored.workspace,
        warnings: Vec::new(),
        project: Ok(runner_core::Project::default()),
    };
    ctx.warnings
        .extend(anchored.warning.map(DetectionWarning::Pipeline));
    resolve(&mut ctx, overrides);

    let mut tasks = std::mem::take(&mut ctx.tasks);
    tasks.sort_by(|a, b| {
        let member_path = |task: &Task| task.member.as_ref().map(|member| member.path.clone());
        a.source
            .provider()
            .caps
            .task_priority
            .cmp(&b.source.provider().caps.task_priority)
            .then_with(|| ctx.scope_rank(a).cmp(&ctx.scope_rank(b)))
            .then_with(|| member_path(a).cmp(&member_path(b)))
            .then_with(|| a.name.cmp(&b.name))
    });
    ctx.tasks = tasks;

    ctx
}

/// The enclosing repository, else the highest ancestor of `dir` strictly
/// below `home` or the filesystem root that is reached through owned
/// directories no one else can write.
fn boundary(dir: &Path, home: Option<&Path>) -> std::path::PathBuf {
    let repository = tool::files::vcs_root(dir);
    let home = home.and_then(|home| home.canonicalize().ok());
    let stops = |path: &Path| {
        repository.as_ref().map_or_else(
            || {
                path.parent().is_none()
                    || home
                        .as_deref()
                        .is_some_and(|home| path.canonicalize().is_ok_and(|path| path == home))
            },
            |repository| !path.starts_with(repository),
        )
    };
    let mut walk = dir.ancestors().take_while(|ancestor| !stops(ancestor));
    walk.next()
        .map_or(dir, |start| {
            walk.take_while(|ancestor| private(ancestor))
                .last()
                .unwrap_or(start)
        })
        .to_path_buf()
}

/// Whether the current user owns `dir` and only the user, or the user's own
/// group, can write to it.
#[cfg(unix)]
fn private(dir: &Path) -> bool {
    use std::os::unix::fs::MetadataExt as _;

    std::fs::metadata(dir).is_ok_and(|meta| {
        meta.uid() == rustix::process::geteuid().as_raw()
            && meta.mode() & 0o002 == 0
            && (meta.mode() & 0o020 == 0 || meta.gid() == rustix::process::getegid().as_raw())
    })
}

/// Whether the current user owns `dir` and only the user, or the user's own
/// group, can write to it.
#[cfg(not(unix))]
fn private(_dir: &Path) -> bool {
    true
}

/// The nearest ancestor inside `boundary` that holds a file any provider's
/// signals name or a config file, else `dir`.
fn project_root(dir: &Path, boundary: &Path) -> std::path::PathBuf {
    dir.ancestors()
        .take_while(|ancestor| ancestor.starts_with(boundary))
        .find(|ancestor| holds_project_files(ancestor))
        .map_or_else(|| dir.to_owned(), Path::to_path_buf)
}

/// The workspace `dir` belongs to, and the declaration that could not be read.
fn anchor(dir: &Path, boundary: &Path) -> (Option<Workspace>, Option<runner_core::Warning>) {
    match runner_core::workspace::anchor(
        dir,
        Some(boundary),
        holds_project_files(dir),
        &runner_providers::REGISTRY,
    ) {
        Ok(workspace) => (workspace.map(workspace_view), None),
        Err(warning) => (None, Some(warning)),
    }
}

/// The CLI's view of a core workspace.
fn workspace_view(workspace: runner_core::Workspace) -> Workspace {
    let members: Vec<Arc<WorkspaceMember>> = workspace
        .members
        .into_iter()
        .map(|member| {
            Arc::new(WorkspaceMember {
                name: member.name,
                path: member.path,
                label: member.label,
                dir: member.dir,
            })
        })
        .collect();
    let current = workspace
        .current
        .and_then(|index| members.get(index).cloned());
    Workspace {
        root: workspace.root,
        kinds: workspace.kinds,
        members,
        current,
    }
}

/// Whether `dir` holds a file a provider signals, or a config file.
fn holds_project_files(dir: &Path) -> bool {
    let signals = || runner_providers::REGISTRY.iter().flat_map(|p| p.signals);
    let exact: Vec<&str> = signals()
        .filter(|signal| match signal {
            runner_core::Signal::ProjectFile { manifests, .. } => {
                manifests.iter().any(|name| dir.join(name).is_file())
            }
            _ => true,
        })
        .flat_map(runner_core::Signal::file_names)
        .collect();
    let caseless: Vec<&str> = signals()
        .filter_map(|signal| match signal {
            runner_core::Signal::FileCaseless(name) => Some(*name),
            _ => None,
        })
        .collect();
    exact.iter().any(|name| dir.join(name).is_file())
        || holds_caseless(dir, &caseless)
        || crate::config::holds_config(dir)
}

/// Whether `dir` holds a file spelled like one of `names` in any ASCII case.
fn holds_caseless(dir: &Path, names: &[&str]) -> bool {
    !names.is_empty()
        && std::fs::read_dir(dir).is_ok_and(|entries| {
            entries.flatten().any(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .is_some_and(|found| names.iter().any(|name| found.eq_ignore_ascii_case(name)))
                    && entry.path().metadata().is_ok_and(|meta| meta.is_file())
            })
        })
}

// Node version

// Observation and resolution

/// The core's project for `ctx`'s tree under `overrides`.
pub(crate) fn observe(
    ctx: &ProjectContext,
    overrides: &crate::resolver::ResolutionOverrides,
) -> std::io::Result<runner_core::Project> {
    let tree = crate::commands::run::core::tree(ctx);
    let evidence = crate::commands::run::core::observe_evidence(&tree)?;
    runner_core::resolve(
        &tree,
        evidence,
        &crate::commands::run::core::policy(overrides, None),
        &runner_providers::REGISTRY,
    )
}

/// Observe and resolve the tree under `overrides`, keeping the project and
/// the tasks and warnings it carries.
fn resolve(ctx: &mut ProjectContext, overrides: &crate::resolver::ResolutionOverrides) {
    let registry = &runner_providers::REGISTRY;
    match observe(ctx, overrides) {
        Ok(project) => {
            ctx.warnings.extend(
                project
                    .warnings
                    .iter()
                    .cloned()
                    .map(DetectionWarning::Pipeline),
            );
            ctx.warnings
                .extend(project.unread.iter().cloned().map(DetectionWarning::Unread));
            for task in &project.tasks {
                let source = crate::provider::task_source(registry.by_id(task.source).label)
                    .expect("registered task source");
                let member = match &task.scope {
                    runner_core::Scope::Root => None,
                    runner_core::Scope::Member { dir, .. } => ctx
                        .workspace
                        .as_ref()
                        .and_then(|workspace| {
                            workspace.members.iter().find(|member| member.dir == *dir)
                        })
                        .cloned(),
                };
                ctx.tasks.push(Task {
                    name: task.name.clone(),
                    source,
                    member,
                    description: task.description.clone(),
                    alias_of: task.alias_of.clone(),
                    run_target: task.target.clone(),
                    passthrough_to: task
                        .forwards_to
                        .and_then(|id| crate::provider::runner(registry.by_id(id).label)),
                    detail: task.detail.clone(),
                });
            }
            ctx.project = Ok(project);
        }
        Err(error) => {
            ctx.warnings
                .push(DetectionWarning::Pipeline(runner_core::Warning::general(
                    error.to_string(),
                )));
            ctx.project = Err(crate::types::Unobserved {
                kind: error.kind(),
                message: error.to_string(),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::process::{Command, Stdio};

    use crate::detect::detect;
    use crate::tool::test_support::TempDir;
    use runner_core::ProviderId;

    /// `git init` + commit the named files. Returns false only when git is
    /// unavailable, so the caller can skip rather than fail.
    fn commit_in(dir: &Path, files: &[&str]) -> bool {
        let available = Command::new("git")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success());
        if !available {
            return false;
        }

        let git = |args: &[&str]| {
            let status = Command::new("git")
                .args(args)
                .current_dir(dir)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .unwrap_or_else(|err| panic!("failed to run `git {}`: {err}", args.join(" ")));
            assert!(
                status.success(),
                "`git {}` failed with {status}",
                args.join(" "),
            );
        };
        git(&["init"]);
        let mut add = vec!["add"];
        add.extend_from_slice(files);
        git(&add);
        git(&["commit", "-m", "lockfiles"]);
        true
    }

    /// A project carrying two node lockfiles.
    fn two_lockfiles(name: &str) -> TempDir {
        let dir = TempDir::new(name);
        fs::write(dir.path().join("package.json"), r#"{"name":"x"}"#).expect("package.json");
        fs::write(dir.path().join("bun.lock"), "").expect("bun.lock");
        fs::write(dir.path().join("package-lock.json"), "{}").expect("package-lock.json");
        dir
    }

    #[test]
    fn detect_records_warnings_for_invalid_task_configs() {
        let dir = TempDir::new("detect-warning");
        fs::write(dir.path().join("turbo.json"), "{").expect("turbo.json should be written");

        let ctx = detect(dir.path(), &crate::resolver::ResolutionOverrides::default());

        assert_eq!(ctx.warnings.len(), 1);
        assert_eq!(ctx.warnings[0].source(), "turbo");
    }

    #[test]
    fn detect_records_warning_for_unparseable_package_manager_field() {
        let dir = TempDir::new("detect-unparseable-pm-field");
        fs::write(
            dir.path().join("package.json"),
            r#"{ "packageManager": "pnpmm@9.0.0" }"#,
        )
        .expect("package.json should be written");

        let ctx = detect(dir.path(), &crate::resolver::ResolutionOverrides::default());

        let detail = ctx
            .warnings
            .iter()
            .find_map(|w| {
                (w.source() == "package.json" && w.detail().contains("packageManager"))
                    .then(|| w.detail())
            })
            .expect("unparseable-packageManager warning should be emitted");
        assert!(
            detail.contains("pnpmm@9.0.0"),
            "warning should echo the raw value verbatim: {detail}",
        );
        assert!(
            detail.contains("npm|pnpm|yarn|bun|deno"),
            "warning should list the accepted values: {detail}",
        );
    }

    #[test]
    fn detect_models_cargo_aliases_as_aliases() {
        let dir = TempDir::new("detect-cargo-alias-shape");
        let cargo_dir = dir.path().join(".cargo");
        fs::create_dir_all(&cargo_dir).expect(".cargo dir should be created");
        fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"fixture\"\nversion = \"0.0.0\"\nedition = \"2024\"\n",
        )
        .expect("Cargo.toml should be written");
        fs::write(
            cargo_dir.join("config.toml"),
            "[alias]\nl = \"clippy --all-targets\"\n",
        )
        .expect("config.toml should be written");

        let ctx = detect(dir.path(), &crate::resolver::ResolutionOverrides::default());
        let task = ctx
            .tasks
            .iter()
            .find(|task| task.source == ProviderId::Cargo && task.name == "l")
            .expect("cargo alias should be detected");

        assert_eq!(task.description, None);
        assert_eq!(task.alias_of.as_deref(), Some("clippy --all-targets"));
    }

    #[test]
    fn detect_models_go_cmd_packages_as_tasks() {
        let dir = TempDir::new("detect-go-cmd-package");
        fs::write(dir.path().join("go.mod"), "module example.com/app\n")
            .expect("go.mod should be written");
        let cmd_dir = dir.path().join("cmd").join("serve");
        fs::create_dir_all(&cmd_dir).expect("cmd package dir should be created");
        fs::write(cmd_dir.join("main.go"), "package main\n\nfunc main() {}\n")
            .expect("main.go should be written");

        let ctx = detect(dir.path(), &crate::resolver::ResolutionOverrides::default());

        assert!(ctx.tasks.iter().any(|task| {
            task.source == ProviderId::Go
                && task.name == "serve"
                && task.run_target.as_deref() == Some("./cmd/serve")
        }));
    }

    #[test]
    fn detect_models_root_go_main_package_as_task() {
        let dir = TempDir::new("detect-go-root-package");
        fs::write(dir.path().join("go.mod"), "module example.com/app\n")
            .expect("go.mod should be written");
        fs::write(
            dir.path().join("main.go"),
            "package main\n\nfunc main() {}\n",
        )
        .expect("main.go should be written");

        let ctx = detect(dir.path(), &crate::resolver::ResolutionOverrides::default());

        // Root task name is the last `module` path segment (deterministic),
        // not the temp directory's randomized name.
        assert!(ctx.tasks.iter().any(|task| {
            task.source == ProviderId::Go
                && task.name == "app"
                && task.run_target.as_deref() == Some(".")
        }));
    }

    #[test]
    fn detect_lists_pyproject_scripts_for_uv_projects() {
        // Headline regression (issue): a uv project's `[project.scripts]`
        // console entry points were detected as a package manager but
        // never surfaced as runnable tasks.
        let dir = TempDir::new("detect-pyproject-scripts-uv");
        fs::write(dir.path().join("uv.lock"), "").expect("uv.lock should be written");
        fs::write(
            dir.path().join("pyproject.toml"),
            "[project]\nname = \"greenpy\"\nversion = \"0.1.0\"\n\n[project.scripts]\nbodysuit = \
             \"greenpy.bodysuit:main\"\ngreenpy = \"greenpy.main:main\"\nnavel-stamper = \
             \"greenpy.navel_stamper:main\"\n",
        )
        .expect("pyproject.toml should be written");

        let ctx = detect(dir.path(), &crate::resolver::ResolutionOverrides::default());

        assert!(ctx.package_managers().contains(&ProviderId::Uv));
        let names: Vec<&str> = ctx
            .tasks
            .iter()
            .filter(|t| t.source == ProviderId::Pyproject)
            .map(|t| t.name.as_str())
            .collect();
        assert_eq!(names, ["bodysuit", "greenpy", "navel-stamper"]);
        // The entry-point target rides along as the task description.
        assert!(ctx.tasks.iter().any(|t| {
            t.source == ProviderId::Pyproject
                && t.name == "greenpy"
                && t.description.as_deref() == Some("greenpy.main:main")
        }));
    }

    #[test]
    fn detect_lists_pyproject_scripts_from_nested_uv_project() {
        let dir = TempDir::new("detect-pyproject-nested-uv");
        fs::create_dir_all(dir.path().join(".git")).expect("git dir should be created");
        let nested = dir.path().join("src").join("pkg");
        fs::create_dir_all(&nested).expect("nested dir should be created");
        fs::write(dir.path().join("uv.lock"), "").expect("uv.lock should be written");
        fs::write(
            dir.path().join("pyproject.toml"),
            "[project]\nname = \"greenpy\"\nversion = \"0.1.0\"\n\n[project.scripts]\ngreenpy = \
             \"greenpy.main:main\"\n",
        )
        .expect("pyproject.toml should be written");

        let ctx = detect(&nested, &crate::resolver::ResolutionOverrides::default());

        assert_eq!(ctx.package_managers(), [ProviderId::Uv]);
        assert!(
            ctx.tasks
                .iter()
                .any(|task| { task.source == ProviderId::Pyproject && task.name == "greenpy" })
        );
    }

    #[test]
    fn detect_lists_pyproject_scripts_without_detected_python_pm() {
        let dir = TempDir::new("detect-pyproject-scripts-no-pm");
        fs::write(
            dir.path().join("pyproject.toml"),
            "[project]\nname = \"greenpy\"\nversion = \"0.1.0\"\n\n[project.scripts]\ngreenpy = \
             \"greenpy.main:main\"\n",
        )
        .expect("pyproject.toml should be written");

        let ctx = detect(dir.path(), &crate::resolver::ResolutionOverrides::default());

        assert!(
            ctx.package_managers().is_empty(),
            "generic pyproject scripts do not imply a specific Python PM",
        );
        assert!(
            ctx.tasks
                .iter()
                .any(|task| { task.source == ProviderId::Pyproject && task.name == "greenpy" })
        );
    }

    #[test]
    fn detect_lists_pyproject_scripts_for_poetry_projects() {
        let dir = TempDir::new("detect-pyproject-scripts-poetry");
        fs::write(
            dir.path().join("pyproject.toml"),
            "[tool.poetry]\nname = \"demo\"\nversion = \"0.1.0\"\n\n[project.scripts]\ncli = \
             \"demo.cli:main\"\n",
        )
        .expect("pyproject.toml should be written");

        let ctx = detect(dir.path(), &crate::resolver::ResolutionOverrides::default());

        assert!(ctx.package_managers().contains(&ProviderId::Poetry));
        assert!(
            ctx.tasks
                .iter()
                .any(|t| { t.source == ProviderId::Pyproject && t.name == "cli" })
        );
    }

    #[test]
    fn the_committed_lockfile_wins_over_the_preference_order() {
        let dir = two_lockfiles("detect-committed-bun");
        fs::write(dir.path().join(".gitignore"), "package-lock.json\n").expect(".gitignore");
        if !commit_in(dir.path(), &["bun.lock", ".gitignore", "package.json"]) {
            eprintln!("skipping: git unavailable");
            return;
        }

        let ctx = detect(dir.path(), &crate::resolver::ResolutionOverrides::default());
        assert_eq!(ctx.package_managers().first(), Some(&ProviderId::Bun));
    }

    #[test]
    fn an_ignored_lockfile_still_wins_when_it_is_the_only_one() {
        // Ignoring a lockfile is a policy about the repository, not a statement
        // that the manager is unused. With nothing to disambiguate, the
        // lockfile that exists is the answer.
        let dir = TempDir::new("detect-ignored-only");
        fs::write(dir.path().join("package.json"), r#"{"name":"x"}"#).expect("package.json");
        fs::write(dir.path().join("bun.lock"), "").expect("bun.lock");
        fs::write(dir.path().join(".gitignore"), "bun.lock\n").expect(".gitignore");
        if !commit_in(dir.path(), &[".gitignore", "package.json"]) {
            eprintln!("skipping: git unavailable");
            return;
        }

        let ctx = detect(dir.path(), &crate::resolver::ResolutionOverrides::default());
        assert_eq!(ctx.package_managers(), vec![ProviderId::Bun]);
    }

    #[test]
    fn two_committed_lockfiles_fall_back_to_the_preference_order() {
        // A repository that commits both is genuinely ambiguous. Detection
        // picks by preference rather than pretending to have evidence.
        let dir = two_lockfiles("detect-both-committed");
        if !commit_in(
            dir.path(),
            &["bun.lock", "package-lock.json", "package.json"],
        ) {
            eprintln!("skipping: git unavailable");
            return;
        }

        let ctx = detect(dir.path(), &crate::resolver::ResolutionOverrides::default());
        assert_eq!(ctx.package_managers().first(), Some(&ProviderId::Npm));
    }

    #[test]
    fn outside_a_repository_the_preference_order_decides() {
        let dir = two_lockfiles("detect-no-git");

        let ctx = detect(dir.path(), &crate::resolver::ResolutionOverrides::default());
        assert_eq!(ctx.package_managers().first(), Some(&ProviderId::Npm));
    }

    #[test]
    fn detect_uses_deno_for_package_json_deno_projects() {
        let dir = TempDir::new("detect-package-json-deno");
        fs::write(
            dir.path().join("package.json"),
            r#"{
  "packageManager": "deno@2.7.12",
  "scripts": {
    "build": "vite build"
  }
}"#,
        )
        .expect("package.json should be written");

        let ctx = detect(dir.path(), &crate::resolver::ResolutionOverrides::default());

        assert_eq!(ctx.package_managers(), [ProviderId::Deno]);
        assert!(
            ctx.tasks
                .iter()
                .any(|task| task.source == ProviderId::PackageJson && task.name == "build")
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_justfile_marks_the_project_root() {
        let dir = TempDir::new("detect-justfile-symlink");
        fs::write(dir.path().join("recipes.just"), "build:\n  echo build\n").expect("target");
        std::os::unix::fs::symlink("recipes.just", dir.path().join("Justfile")).expect("symlink");
        assert!(super::holds_caseless(dir.path(), &["justfile"]));
        assert!(!super::holds_caseless(dir.path(), &["makefile"]));
    }

    #[test]
    fn the_project_root_is_found_from_a_child_directory_for_every_signal_kind() {
        for (name, body, task) in [
            (
                "deno.json",
                r#"{"tasks":{"build":"deno run build.ts"}}"#,
                "build",
            ),
            (
                "pyproject.toml",
                "[project]\nname = \"demo\"\n\n[project.scripts]\ngreenpy = \"greenpy:main\"\n",
                "greenpy",
            ),
            ("JUSTFILE", "build:\n  echo build\n", "build"),
        ] {
            let dir = TempDir::new("detect-root-signal");
            fs::write(dir.path().join(name), body).expect("config");
            let src = dir.path().join("src");
            fs::create_dir_all(&src).expect("src");
            if !commit_in(dir.path(), &[name]) {
                eprintln!("skipping: git unavailable");
                return;
            }
            let ctx = detect(&src, &crate::resolver::ResolutionOverrides::default());
            assert_eq!(ctx.root, dir.path(), "{name}");
            if name == "JUSTFILE" && runner_core::probe_with("just", &[]).is_none() {
                continue;
            }
            assert!(
                ctx.tasks.iter().any(|found| found.name == task),
                "{name}: {:?}",
                ctx.tasks
            );
        }
    }

    #[test]
    fn yarn_configuration_alone_does_not_hide_an_ancestor_workspace() {
        for config in [".yarnrc", ".yarnrc.yml"] {
            let dir = TempDir::new("detect-yarn-condition");
            fs::write(
                dir.path().join("package.json"),
                r#"{"workspaces":["packages/*"],"scripts":{"build":"echo root"}}"#,
            )
            .unwrap();
            let loose = dir.path().join("loose");
            fs::create_dir_all(&loose).unwrap();
            fs::write(loose.join(config), "").unwrap();
            assert!(!super::holds_project_files(&loose));
            assert_eq!(super::project_root(&loose, dir.path()), dir.path());
            let (workspace, warning) = super::anchor(&loose, dir.path());
            assert!(warning.is_none());
            assert_eq!(workspace.unwrap().root, dir.path());
            for manifest in ["package.json", "package.json5", "package.yaml"] {
                fs::write(loose.join(manifest), "{}").unwrap();
                assert!(super::holds_project_files(&loose));
                assert_eq!(super::project_root(&loose, dir.path()), loose);
                fs::remove_file(loose.join(manifest)).unwrap();
            }
        }
    }

    #[test]
    fn a_config_file_alone_marks_the_project_root() {
        for name in [
            "runner.toml",
            ".runner.toml",
            ".config/runner.toml",
            ".config/.runner.toml",
        ] {
            let dir = TempDir::new("detect-root-config");
            fs::create_dir_all(dir.path().join(".config")).expect(".config");
            fs::write(dir.path().join(name), "download = false\n").expect("config");
            let src = dir.path().join("src");
            fs::create_dir_all(&src).expect("src");
            if !commit_in(dir.path(), &[name]) {
                eprintln!("skipping: git unavailable");
                return;
            }
            let root = crate::detect::anchored(&src).root;
            assert_eq!(root, dir.path(), "{name}");
            let loaded = crate::config::load(&root)
                .expect("parses")
                .expect("present");
            assert!(
                loaded.path.ends_with(name),
                "{name}: {}",
                loaded.path.display()
            );
        }
    }

    #[test]
    fn detect_uses_nearest_deno_sources_from_nested_dir() {
        let dir = TempDir::new("detect-deno-nearest");
        let nested = dir.path().join("apps").join("site").join("src");
        fs::create_dir_all(&nested).expect("nested dir should be created");
        fs::write(dir.path().join("deno.lock"), "{}").expect("deno.lock should be written");
        fs::write(
            dir.path().join("deno.jsonc"),
            r#"{ workspace: ["apps/site"], tasks: { root: "deno task root" } }"#,
        )
        .expect("root deno.jsonc should be written");
        fs::write(
            dir.path().join("apps").join("site").join("package.json"),
            r#"{
  "scripts": {
    "member": "deno task member"
  }
}"#,
        )
        .expect("member package.json should be written");

        let ctx = detect(&nested, &crate::resolver::ResolutionOverrides::default());

        assert!(ctx.package_managers().contains(&ProviderId::Deno));
        assert!(ctx.tasks.iter().any(|task| task.name == "member"));
        assert!(ctx.tasks.iter().any(|task| task.name == "root"));
    }

    #[test]
    fn detect_lists_scripts_without_lockfile_or_pm_field() {
        // A `package.json` with scripts but no lockfile and no
        // `packageManager`/`devEngines` field (a typical pnpm-workspace
        // member) must still list its scripts despite detecting no PM.
        let dir = TempDir::new("detect-scripts-no-pm-signal");
        fs::write(
            dir.path().join("package.json"),
            r#"{ "name": "leaf", "scripts": { "build": "wxt build" } }"#,
        )
        .expect("package.json should be written");

        let ctx = detect(dir.path(), &crate::resolver::ResolutionOverrides::default());

        assert!(
            ctx.package_managers().is_empty(),
            "no lockfile/pm field → no PM detected, yet scripts must still list",
        );
        assert!(
            ctx.tasks
                .iter()
                .any(|task| task.source == ProviderId::PackageJson && task.name == "build")
        );
    }

    #[test]
    fn detect_lists_workspace_member_scripts_from_manifestless_subdir() {
        // Workspace-root-aware upward walk: a manifest-less subdir inside
        // a monorepo (root `pnpm-workspace.yaml`) adopts the nearest
        // ancestor manifest's scripts.
        let dir = TempDir::new("detect-workspace-member-subdir");
        fs::create_dir_all(dir.path().join(".git")).expect("git dir should be created");
        fs::write(
            dir.path().join("pnpm-workspace.yaml"),
            "packages:\n  - apps/*\n",
        )
        .expect("pnpm-workspace.yaml should be written");
        let member = dir.path().join("apps").join("ext");
        let nested = member.join("src");
        fs::create_dir_all(&nested).expect("nested dir should be created");
        fs::write(
            member.join("package.json"),
            r#"{ "scripts": { "ext-build": "wxt build" } }"#,
        )
        .expect("member package.json should be written");

        let ctx = detect(&nested, &crate::resolver::ResolutionOverrides::default());

        assert!(
            ctx.tasks
                .iter()
                .any(|task| task.source == ProviderId::PackageJson && task.name == "ext-build")
        );
    }

    #[test]
    fn outside_a_repository_an_ancestor_manifest_is_the_root() {
        let dir = TempDir::new("detect-no-repository");
        fs::write(
            dir.path().join("package.json"),
            r#"{ "scripts": { "parent": "echo" } }"#,
        )
        .expect("ancestor package.json should be written");
        let sub = dir.path().join("sub");
        fs::create_dir_all(&sub).expect("subdir should be created");

        let ctx = detect(&sub, &crate::resolver::ResolutionOverrides::default());

        assert_eq!(ctx.root, dir.path());
        assert!(
            ctx.tasks
                .iter()
                .any(|task| task.name == "parent" && task.member.is_none())
        );
    }

    #[test]
    fn outside_a_repository_an_ancestor_pyproject_is_the_root() {
        let dir = TempDir::new("detect-no-repository-pyproject");
        fs::write(
            dir.path().join("pyproject.toml"),
            "[project]\nname = \"demo\"\n\n[project.scripts]\nserve = \"demo:main\"\n",
        )
        .expect("ancestor pyproject.toml should be written");
        let sub = dir.path().join("src").join("demo");
        fs::create_dir_all(&sub).expect("subdir should be created");

        let ctx = detect(&sub, &crate::resolver::ResolutionOverrides::default());

        assert_eq!(ctx.root, dir.path());
        assert!(ctx.tasks.iter().any(|task| task.name == "serve"));
    }

    #[test]
    fn outside_a_repository_the_walk_stops_below_home() {
        let home = TempDir::new("detect-no-repository-home");
        fs::write(
            home.path().join("package.json"),
            r#"{ "scripts": { "home": "echo" } }"#,
        )
        .expect("home package.json should be written");
        fs::write(
            home.path().join("pnpm-workspace.yaml"),
            "packages:\n  - \"*\"\n",
        )
        .expect("home workspace should be written");
        let sub = home.path().join("proj").join("sub");
        fs::create_dir_all(&sub).expect("subdir should be created");

        let ctx = super::detect_anchored(
            &sub,
            super::anchored_below(&sub, Some(home.path())),
            &crate::resolver::ResolutionOverrides::default(),
        );

        assert_eq!(ctx.root, sub);
        assert!(ctx.workspace.is_none());
        assert!(ctx.tasks.iter().all(|task| task.name != "home"));
    }

    #[cfg(unix)]
    fn parent_with_mode(prefix: &str, mode: u32) -> (TempDir, std::path::PathBuf) {
        use std::os::unix::fs::PermissionsExt as _;

        let parent = TempDir::new(prefix);
        fs::write(
            parent.path().join("package.json"),
            r#"{ "scripts": { "parent": "echo" } }"#,
        )
        .expect("parent package.json should be written");
        fs::write(
            parent.path().join("pnpm-workspace.yaml"),
            "packages:\n  - \"*\"\n",
        )
        .expect("parent workspace should be written");
        fs::set_permissions(parent.path(), fs::Permissions::from_mode(mode))
            .expect("parent mode should be set");
        let sub = parent.path().join("child").join("sub");
        fs::create_dir_all(&sub).expect("subdir should be created");
        (parent, sub)
    }

    #[cfg(unix)]
    #[test]
    fn outside_a_repository_a_world_writable_ancestor_is_never_the_root() {
        let (_parent, sub) = parent_with_mode("detect-no-repository-shared", 0o777);

        let ctx = detect(&sub, &crate::resolver::ResolutionOverrides::default());

        assert_eq!(ctx.root, sub);
        assert!(ctx.workspace.is_none());
        assert!(ctx.tasks.iter().all(|task| task.name != "parent"));
    }

    #[cfg(unix)]
    #[test]
    fn outside_a_repository_a_private_ancestor_is_the_root() {
        let (parent, sub) = parent_with_mode("detect-no-repository-private", 0o755);

        let ctx = detect(&sub, &crate::resolver::ResolutionOverrides::default());

        assert_eq!(ctx.root, parent.path());
        assert!(ctx.tasks.iter().any(|task| task.name == "parent"));
    }

    #[cfg(unix)]
    #[test]
    fn a_repository_in_a_world_writable_dir_is_never_the_root() {
        let (parent, sub) = parent_with_mode("detect-repository-shared", 0o777);
        fs::create_dir_all(parent.path().join(".git")).expect("git dir should be created");

        let ctx = detect(&sub, &crate::resolver::ResolutionOverrides::default());

        assert_eq!(ctx.root, sub);
        assert!(ctx.workspace.is_none());
        assert!(ctx.tasks.iter().all(|task| task.name != "parent"));
    }

    #[cfg(unix)]
    #[test]
    fn an_ancestor_only_the_users_own_group_can_write_is_the_root() {
        let (parent, sub) = parent_with_mode("detect-no-repository-own-group", 0o775);

        let ctx = detect(&sub, &crate::resolver::ResolutionOverrides::default());

        assert_eq!(ctx.root, parent.path());
        assert!(ctx.tasks.iter().any(|task| task.name == "parent"));
    }

    #[cfg(unix)]
    #[test]
    fn a_world_writable_invocation_dir_is_still_its_own_root() {
        let (parent, _sub) = parent_with_mode("detect-no-repository-shared-cwd", 0o777);

        let ctx = detect(
            parent.path(),
            &crate::resolver::ResolutionOverrides::default(),
        );

        assert_eq!(ctx.root, parent.path());
        assert!(ctx.tasks.iter().any(|task| task.name == "parent"));
    }

    #[test]
    fn detect_lists_an_ancestor_manifest_inside_the_tree_in_root_scope() {
        let dir = TempDir::new("detect-no-workspace-no-adopt");
        fs::create_dir_all(dir.path().join(".git")).expect("git dir should be created");
        fs::write(
            dir.path().join("package.json"),
            r#"{ "scripts": { "root-only": "echo nope" } }"#,
        )
        .expect("ancestor package.json should be written");
        let sub = dir.path().join("sub");
        fs::create_dir_all(&sub).expect("subdir should be created");

        let ctx = detect(&sub, &crate::resolver::ResolutionOverrides::default());

        assert!(
            ctx.tasks
                .iter()
                .any(|task| task.name == "root-only" && task.member.is_none()),
            "an ancestor manifest below the project root is a root-scoped task; got {:?}",
            ctx.tasks.iter().map(|task| &task.name).collect::<Vec<_>>(),
        );
    }

    #[test]
    fn detect_pm_from_dev_engines_without_lockfile() {
        // devEngines-only manifest (no lockfile, no legacy
        // packageManager) must resolve a node PM so info/install work.
        let dir = TempDir::new("detect-dev-engines-pm");
        fs::write(
            dir.path().join("package.json"),
            r#"{ "devEngines": { "packageManager": { "name": "pnpm", "version": "9" } },
                 "scripts": { "build": "vite build" } }"#,
        )
        .expect("package.json should be written");

        let ctx = detect(dir.path(), &crate::resolver::ResolutionOverrides::default());

        assert_eq!(ctx.package_managers(), [ProviderId::Pnpm]);
        assert!(
            ctx.tasks
                .iter()
                .any(|task| task.source == ProviderId::PackageJson && task.name == "build")
        );
    }

    #[test]
    fn detect_pm_upwards_for_workspace_member() {
        // A member dir with its own lockfile-less, PM-less package.json
        // inside a pnpm workspace whose root carries the lockfile: the
        // member must inherit the root's pnpm so `runner install` here
        // doesn't fall back to the wrong manager.
        let dir = TempDir::new("detect-pm-upwards-member");
        fs::create_dir_all(dir.path().join(".git")).expect("git dir should be created");
        fs::write(
            dir.path().join("pnpm-workspace.yaml"),
            "packages:\n  - apps/*\n",
        )
        .expect("pnpm-workspace.yaml should be written");
        fs::write(
            dir.path().join("pnpm-lock.yaml"),
            "lockfileVersion: '9.0'\n",
        )
        .expect("root pnpm-lock.yaml should be written");
        let member = dir.path().join("apps").join("ext");
        fs::create_dir_all(&member).expect("member dir should be created");
        fs::write(
            member.join("package.json"),
            r#"{ "name": "ext", "scripts": { "build": "wxt build" } }"#,
        )
        .expect("member package.json should be written");

        let ctx = detect(&member, &crate::resolver::ResolutionOverrides::default());

        assert_eq!(ctx.package_managers(), [ProviderId::Pnpm]);
        assert!(
            ctx.tasks
                .iter()
                .any(|task| task.source == ProviderId::PackageJson && task.name == "build")
        );
    }

    #[test]
    fn a_workspace_scoped_entry_is_the_task_in_that_member_scope() {
        let dir = TempDir::new("turbo-mixed-keys");
        fs::create_dir_all(dir.path().join(".git")).expect("git dir should be created");
        fs::write(
            dir.path().join("pnpm-workspace.yaml"),
            "packages:\n  - apps/*\n",
        )
        .expect("pnpm-workspace.yaml should be written");
        let member = dir.path().join("apps").join("web");
        fs::create_dir_all(&member).expect("member dir should be created");
        fs::write(member.join("package.json"), r#"{ "name": "web" }"#)
            .expect("member package.json should be written");
        fs::write(
            dir.path().join("turbo.json"),
            r#"{"tasks":{"build":{},"//#lint":{},"//#format":{"cache":false},"web#build":{}}}"#,
        )
        .expect("turbo.json should be written");

        let ctx = detect(dir.path(), &crate::resolver::ResolutionOverrides::default());
        let mut tasks: Vec<(&str, &str)> = ctx
            .tasks
            .iter()
            .filter(|task| task.source == ProviderId::Turbo)
            .map(|task| (task.name.as_str(), task.scope()))
            .collect();
        tasks.sort_unstable();

        assert_eq!(
            tasks,
            [
                ("build", "root"),
                ("build", "web"),
                ("format", "root"),
                ("lint", "root"),
            ]
        );
    }
}
