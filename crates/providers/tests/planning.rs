//! Regression tests for the architecture's planning contracts against real providers.

use std::sync::atomic::{AtomicUsize, Ordering};

use runner_core::{
    Cascade, Dispatch, Evidence, OnMismatch, Op, Policy, Present, Project, ProviderId, ReachPolicy,
    Refusal, Scope, ScriptPolicy, SignalId, Tree, TrustPolicy, Unsafe, Weight, dispatch_from,
    plan_found, plan_with,
};
use runner_providers::REGISTRY;

struct Fixture(Tree);

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "runner-provider-planning-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).unwrap();
        Self(Tree {
            cwd: root.clone(),
            root,
            members: vec![],
        })
    }

    fn present(&self, provider: ProviderId) -> Present {
        Present {
            provider,
            scope: Scope::Root,
            version: None,
            bin_dirs: vec![],
            because: vec![Evidence {
                provider: Some(provider),
                signal: Some(SignalId(0)),
                at: self.0.root.clone(),
                scope: Scope::Root,
                weight: Weight::Configured,
                declared: None,
            }],
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0.root);
    }
}

#[test]
fn local_exec_precedes_a_fetching_manager() {
    let fixture = Fixture::new();
    let project = Project {
        present: vec![
            fixture.present(ProviderId::Mise),
            fixture.present(ProviderId::Yarn),
        ],
        ..Project::default()
    };
    let policy = Policy {
        reach: ReachPolicy::Local,
        ..Policy::default()
    };
    let cascade = Cascade {
        tree: &fixture.0,
        project: &project,
        policy: &policy,
        registry: &REGISTRY,
        builtins: &[],
        dep: None,
        confirm: None,
    };
    let (rung, Dispatch::Plan(plan)) =
        dispatch_from(&cascade, "test", "runner-audit-local-yarn-target", &[]).unwrap()
    else {
        panic!("expected plan")
    };
    assert_eq!(rung.name, "local-exec");
    assert_eq!(plan.provider, Some(ProviderId::Yarn));
    assert_eq!(
        plan.argv,
        ["yarn", "run", "runner-audit-local-yarn-target"].map(std::ffi::OsString::from)
    );
}

#[test]
fn pnpm_exec_runs_under_a_local_fetch_policy() {
    let fixture = Fixture::new();
    let project = Project {
        present: vec![fixture.present(ProviderId::Pnpm)],
        ..Project::default()
    };
    let policy = Policy {
        reach: ReachPolicy::Local,
        ..Policy::default()
    };
    let cascade = Cascade {
        tree: &fixture.0,
        project: &project,
        policy: &policy,
        registry: &REGISTRY,
        builtins: &[],
        dep: None,
        confirm: None,
    };
    let (rung, Dispatch::Plan(plan)) =
        dispatch_from(&cascade, "test", "runner-audit-local-pnpm-target", &[]).unwrap()
    else {
        panic!("expected plan")
    };
    assert_eq!(rung.name, "local-exec");
    assert_eq!(
        plan.argv,
        ["pnpm", "exec", "runner-audit-local-pnpm-target"].map(std::ffi::OsString::from)
    );
}

#[test]
fn discovered_binary_has_evidence_and_no_unrelated_tool_environment() {
    let fixture = Fixture::new();
    let found = fixture.0.root.join("binary");
    std::fs::write(&found, "").unwrap();
    let mut policy = Policy::default();
    policy
        .env
        .project
        .insert("PROJECT_KEY".into(), "present".into());
    policy
        .env
        .tool
        .entry(ProviderId::Npm)
        .or_default()
        .insert("NPM_ONLY".into(), "absent".into());
    let plan = plan_found(
        &fixture.0,
        &Project::default(),
        &policy,
        found.clone(),
        &REGISTRY,
        &[],
    )
    .unwrap();
    assert_eq!(plan.because.len(), 1);
    assert_eq!(plan.because[0].at, found);
    assert_eq!(plan.because[0].provider, None);
    assert_eq!(plan.env, vec![("PROJECT_KEY".into(), "present".into())]);
    policy
        .env
        .project
        .insert("NODE_OPTIONS".into(), "--placeholder".into());
    assert!(matches!(
        plan_found(
            &fixture.0,
            &Project::default(),
            &policy,
            found.clone(),
            &REGISTRY,
            &[]
        ),
        Err(Refusal::Unsafe(Unsafe::LoaderHook { .. }))
    ));
    policy.trust = TrustPolicy::Full;
    assert_eq!(
        plan_found(
            &fixture.0,
            &Project::default(),
            &policy,
            found,
            &REGISTRY,
            &[]
        )
        .unwrap()
        .env
        .len(),
        2
    );
}

#[test]
fn frozen_tool_install_requires_its_declared_lockfile() {
    let fixture = Fixture::new();
    std::fs::write(fixture.0.root.join("mise.toml"), "").unwrap();
    let policy = Policy {
        frozen: true,
        ..Policy::default()
    };
    let present = fixture.present(ProviderId::Mise);
    let project = Project::default();
    let operations = ["install".into()];
    let op = Op::Install {
        operations: &operations,
    };
    let make = || plan_with(&fixture.0, &project, &policy, &present, &op, &REGISTRY).unwrap();
    assert!(!make().argv.contains(&"--locked".into()));
    std::fs::write(fixture.0.root.join("mise.lock"), "").unwrap();
    assert!(make().argv.contains(&"--locked".into()));
}

#[test]
fn a_frozen_install_refuses_until_the_lockfile_it_would_pin_exists() {
    let frozen = Policy {
        frozen: true,
        ..Policy::default()
    };
    let install = Op::Install { operations: &[] };
    let project = Project::default();
    for (id, lockfiles, frozen_form) in [
        (
            ProviderId::Npm,
            &["package-lock.json", "npm-shrinkwrap.json"][..],
            "ci",
        ),
        (ProviderId::Pnpm, &["pnpm-lock.yaml"], "--frozen-lockfile"),
        (
            ProviderId::Bun,
            &["bun.lock", "bun.lockb"],
            "--frozen-lockfile",
        ),
        (ProviderId::Yarn, &["yarn.lock"], "--frozen-lockfile"),
        (ProviderId::Deno, &["deno.lock"], "--frozen"),
        (ProviderId::Uv, &["uv.lock"], "--frozen"),
        (ProviderId::Pipenv, &["Pipfile.lock"], "sync"),
        (ProviderId::Cargo, &["Cargo.lock"], "--locked"),
    ] {
        let fixture = Fixture::new();
        let present = fixture.present(id);
        let planned = |policy: &Policy| {
            plan_with(&fixture.0, &project, policy, &present, &install, &REGISTRY)
        };
        assert_eq!(
            planned(&frozen),
            Err(Refusal::NoLockfile {
                provider: id,
                dir: fixture.0.root.clone(),
                lockfiles: lockfiles.iter().map(|name| (*name).to_owned()).collect(),
            }),
            "{id:?}"
        );
        let unfrozen = planned(&Policy::default()).unwrap();
        assert!(
            !unfrozen.argv.contains(&frozen_form.into()),
            "{id:?}: {unfrozen:?}"
        );
        std::fs::write(fixture.0.root.join(lockfiles[0]), "").unwrap();
        let locked = planned(&frozen).unwrap();
        assert!(
            locked.argv.contains(&frozen_form.into()),
            "{id:?}: {locked:?}"
        );
    }
    for id in [
        ProviderId::Composer,
        ProviderId::Go,
        ProviderId::Bundler,
        ProviderId::Poetry,
    ] {
        let fixture = Fixture::new();
        let present = fixture.present(id);
        plan_with(&fixture.0, &project, &frozen, &present, &install, &REGISTRY)
            .unwrap_or_else(|refusal| panic!("{id:?}: {refusal:?}"));
    }
}

#[test]
fn a_frozen_install_accepts_alternative_and_configured_lockfiles() {
    let frozen = Policy {
        frozen: true,
        ..Policy::default()
    };
    let install = Op::Install { operations: &[] };
    let project = Project::default();
    for (id, files, frozen_form) in [
        (ProviderId::Npm, &[("npm-shrinkwrap.json", "{}")][..], "ci"),
        (
            ProviderId::Deno,
            &[
                (
                    "deno.jsonc",
                    r#"{ /* pinned */ "lock": { "path": "deps.lock" } }"#,
                ),
                ("deps.lock", "{}"),
            ],
            "--frozen",
        ),
        (
            ProviderId::Deno,
            &[
                ("deno.json", r#"{ "lock": "deps.lock" }"#),
                ("deps.lock", "{}"),
            ],
            "--frozen",
        ),
    ] {
        let fixture = Fixture::new();
        for (name, body) in files {
            std::fs::write(fixture.0.root.join(name), body).unwrap();
        }
        let present = fixture.present(id);
        let locked = plan_with(&fixture.0, &project, &frozen, &present, &install, &REGISTRY)
            .unwrap_or_else(|refusal| panic!("{id:?}: {refusal:?}"));
        assert!(
            locked.argv.contains(&frozen_form.into()),
            "{id:?}: {locked:?}"
        );
    }
    let fixture = Fixture::new();
    std::fs::write(
        fixture.0.root.join("deno.json"),
        r#"{ "lock": "deps.lock" }"#,
    )
    .unwrap();
    let present = fixture.present(ProviderId::Deno);
    assert_eq!(
        plan_with(&fixture.0, &project, &frozen, &present, &install, &REGISTRY),
        Err(Refusal::NoLockfile {
            provider: ProviderId::Deno,
            dir: fixture.0.root.clone(),
            lockfiles: vec!["deno.lock".to_owned(), "deps.lock".to_owned()],
        })
    );
}

#[test]
fn unsupported_and_allowlist_script_policies_are_clamped() {
    let fixture = Fixture::new();
    for (id, scripts, clamped) in [
        (ProviderId::Bun, ScriptPolicy::Allow, true),
        (ProviderId::Cargo, ScriptPolicy::Deny, true),
        (ProviderId::Cargo, ScriptPolicy::Allow, false),
        (ProviderId::Npm, ScriptPolicy::Deny, false),
    ] {
        let policy = Policy {
            scripts,
            ..Policy::default()
        };
        let plan = plan_with(
            &fixture.0,
            &Project::default(),
            &policy,
            &fixture.present(id),
            &Op::Install { operations: &[] },
            &REGISTRY,
        )
        .unwrap();
        assert_eq!(!plan.clamps.is_empty(), clamped, "{id:?}");
    }
}

#[test]
fn loader_refusals_cannot_fall_through_to_another_exec_provider() {
    let fixture = Fixture::new();
    let project = Project {
        present: vec![fixture.present(ProviderId::Bun)],
        ..Project::default()
    };
    let mut policy = Policy {
        reach: ReachPolicy::Allow,
        ..Policy::default()
    };
    policy
        .env
        .project
        .insert("NODE_OPTIONS".into(), "--placeholder".into());
    let cascade = Cascade {
        tree: &fixture.0,
        project: &project,
        policy: &policy,
        registry: &REGISTRY,
        builtins: &[],
        dep: None,
        confirm: None,
    };
    for token in ["test", "runner-audit-no-such-binary"] {
        assert!(matches!(
            dispatch_from(&cascade, "test", token, &[]),
            Err(Refusal::Unsafe(Unsafe::LoaderHook { .. }))
        ));
    }
}

#[test]
fn failed_read_only_queries_are_errors_with_provider_and_scope() {
    fn fails(_: &std::path::Path) -> std::io::Result<Vec<Evidence>> {
        Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "query denied",
        ))
    }
    static PROVIDERS: &[runner_core::Provider] = &[runner_core::Provider {
        signals: &[runner_core::Signal::Ask(fails)],
        ..runner_providers::managers::mise::PROVIDER
    }];
    let fixture = Fixture::new();
    let error = runner_core::observe(&fixture.0, &runner_core::Registry(PROVIDERS)).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
    let message = error.to_string();
    assert!(message.contains("mise") && message.contains("query denied"));
    assert!(message.contains(fixture.0.root.to_str().unwrap()));
}

#[test]
fn yarn_observation_selects_classic_or_berry_local_exec() {
    for (lock, expected) in [
        ("# yarn lockfile v1\n", "run"),
        ("__metadata:\n  version: 8\n", "exec"),
    ] {
        let fixture = Fixture::new();
        std::fs::write(fixture.0.root.join("yarn.lock"), lock).unwrap();
        let policy = Policy {
            reach: ReachPolicy::Local,
            ..Policy::default()
        };
        let evidence = runner_core::observe(&fixture.0, &REGISTRY).unwrap();
        let project = runner_core::resolve(&fixture.0, evidence, &policy, &REGISTRY).unwrap();
        let cascade = Cascade {
            tree: &fixture.0,
            project: &project,
            policy: &policy,
            registry: &REGISTRY,
            builtins: &[],
            dep: None,
            confirm: None,
        };
        let (rung, Dispatch::Plan(plan)) =
            runner_core::dispatch(&cascade, "runner-variant-test-not-on-path", &[]).unwrap()
        else {
            panic!("expected plan")
        };
        assert_eq!(rung.name, "local-exec");
        assert_eq!(plan.argv[0], "yarn");
        assert_eq!(plan.argv[1], expected);
    }
}

#[test]
fn file_fallback_is_selected_from_registry_without_inventing_project_presence() {
    let fixture = Fixture::new();
    let project = Project::default();
    let policy = Policy::default();
    for (file, program) in [
        ("main.js", "node"),
        ("main.py", if cfg!(windows) { "python" } else { "python3" }),
    ] {
        let path = fixture.0.root.join(file);
        std::fs::write(&path, "").unwrap();
        let cascade = Cascade {
            tree: &fixture.0,
            project: &project,
            policy: &policy,
            registry: &REGISTRY,
            builtins: &[],
            dep: None,
            confirm: None,
        };
        let (rung, Dispatch::Plan(plan)) = runner_core::dispatch(&cascade, file, &[]).unwrap()
        else {
            panic!("expected file plan")
        };
        assert_eq!(rung.name, "file");
        assert_eq!(plan.argv[0], program);
        assert_eq!(plan.because[0].at, path);
        assert!(plan.because[0].provider.is_none());
        assert_eq!(project.present.len(), 0);
    }
}

#[test]
fn yarn_derived_observations_preserve_read_and_parse_errors() {
    for (name, content) in [
        ("package.json", None),
        ("yarn.lock", None),
        ("package.json", Some(b"not json".as_slice())),
        ("yarn.lock", Some(b"\xff".as_slice())),
    ] {
        let fixture = Fixture::new();
        let path = fixture.0.root.join(name);
        if let Some(content) = content {
            std::fs::write(&path, content).unwrap();
        } else {
            std::fs::create_dir(&path).unwrap();
        }
        let mut evidence = fixture.present(ProviderId::Yarn).because;
        let error = runner_core::observe::derive(&fixture.0, &REGISTRY, &mut evidence).unwrap_err();
        assert!(error.to_string().contains("yarn observation failed"));
        assert!(error.to_string().contains(&path.display().to_string()));
        assert!(
            evidence
                .iter()
                .all(|e| !matches!(e.declared, Some(runner_core::Declared::Variant(_))))
        );
    }
}

#[test]
fn yarn_missing_optional_files_are_classic_and_berry_evidence_names_its_file() {
    let fixture = Fixture::new();
    let mut evidence = fixture.present(ProviderId::Yarn).because;
    runner_core::observe::derive(&fixture.0, &REGISTRY, &mut evidence).unwrap();
    assert_eq!(evidence.len(), 1);
    let manifest = fixture.0.root.join("package.json");
    std::fs::write(&manifest, r#"{"packageManager":"yarn@4.0.0"}"#).unwrap();
    runner_core::observe::derive(&fixture.0, &REGISTRY, &mut evidence).unwrap();
    assert!(evidence.iter().any(|e| e.at == manifest
        && matches!(&e.declared, Some(runner_core::Declared::Variant(name)) if name == "berry")));
}

#[test]
fn registered_runtimes_plan_supported_sources_and_forward_arguments() {
    let fixture = Fixture::new();
    for (provider, file, prefix) in [
        (ProviderId::Node, "main.js", vec!["node"]),
        (ProviderId::Node, "main.MTS", vec!["node"]),
        (ProviderId::Bun, "view.tsx", vec!["bun"]),
        (ProviderId::Deno, "view.jsx", vec!["deno", "run"]),
        (ProviderId::Go, "main.go", vec!["go", "run"]),
        (ProviderId::Uv, "main.py", vec!["uv", "run"]),
        (
            ProviderId::Python,
            "main.py",
            vec![if cfg!(windows) { "python" } else { "python3" }],
        ),
    ] {
        let path = fixture.0.root.join(file);
        std::fs::write(&path, "").unwrap();
        let policy = Policy::default();
        let project = Project {
            present: vec![fixture.present(provider)],
            ..Project::default()
        };
        let cascade = Cascade {
            tree: &fixture.0,
            project: &project,
            policy: &policy,
            registry: &REGISTRY,
            builtins: &[],
            dep: None,
            confirm: None,
        };
        for token in [file.to_owned(), format!("./{file}")] {
            let (_, Dispatch::Plan(plan)) =
                runner_core::dispatch(&cascade, &token, &["a b".into()]).unwrap()
            else {
                panic!("file plan")
            };
            let expected: Vec<std::ffi::OsString> = prefix
                .iter()
                .map(Into::into)
                .chain([path.as_os_str().to_owned(), "a b".into()])
                .collect();
            assert_eq!(plan.argv, expected, "{provider:?}");
        }
    }
}

#[test]
fn directories_and_remote_specs_reach_the_provider_exec_rung() {
    let fixture = Fixture::new();
    std::fs::create_dir_all(fixture.0.root.join("cmd/tool")).unwrap();
    let project = Project {
        present: vec![fixture.present(ProviderId::Go)],
        ..Project::default()
    };
    let policy = Policy {
        reach: ReachPolicy::Allow,
        ..Policy::default()
    };
    let cascade = Cascade {
        tree: &fixture.0,
        project: &project,
        policy: &policy,
        registry: &REGISTRY,
        builtins: &[],
        dep: None,
        confirm: None,
    };
    for token in ["./cmd/tool", "github.com/acme/missing-tool@v1"] {
        let (rung, Dispatch::Plan(plan)) =
            runner_core::dispatch(&cascade, token, &["a b".into()]).unwrap()
        else {
            panic!("expected a provider plan");
        };
        assert_eq!(rung.name, "exec");
        assert_eq!(plan.provider, Some(ProviderId::Go));
        assert_eq!(
            plan.argv,
            ["go", "run", token, "a b"].map(std::ffi::OsString::from)
        );
    }
}

#[test]
fn shebang_parsing_preserves_kernel_and_env_split_argument_boundaries() {
    let fixture = Fixture::new();
    let path = fixture.0.root.join("script");
    for (line, program, args) in [
        (
            "#!/usr/bin/python3 -O -u",
            "/usr/bin/python3",
            vec!["-O -u"],
        ),
        (
            "#!/usr/bin/env node --trace-warnings",
            "node --trace-warnings",
            vec![],
        ),
        (
            "#!/usr/bin/env -S deno run --allow-read='a b'",
            "deno",
            vec!["run", "--allow-read=a b"],
        ),
        (
            "#!/usr/bin/env --split-string=deno run",
            "deno",
            vec!["run"],
        ),
        (
            "#!/usr/bin/env --split-string deno run",
            "deno",
            vec!["run"],
        ),
        ("#!/usr/bin/env -Sdeno run", "deno", vec!["run"]),
    ] {
        std::fs::write(&path, format!("{line}\n")).unwrap();
        let shebang = runner_core::read_shebang(&path).unwrap();
        assert_eq!(shebang.program, program, "{line}");
        assert_eq!(shebang.args, args, "{line}");
    }
}

#[test]
#[cfg(unix)]
fn file_planning_distinguishes_executable_sources_shebangs_and_native_files() {
    use std::os::unix::fs::PermissionsExt as _;
    let fixture = Fixture::new();
    let project = Project::default();
    let policy = Policy::default();
    let cascade = Cascade {
        tree: &fixture.0,
        project: &project,
        policy: &policy,
        registry: &REGISTRY,
        builtins: &[],
        dep: None,
        confirm: None,
    };
    for (name, content, mode, expected_program) in [
        ("main.js", "console.log(1)", 0o755, "node"),
        ("script", "#!/usr/bin/env -S bash -e\n", 0o644, "bash"),
        ("script", "#!/bin/sh\n", 0o755, "script"),
        ("native", "", 0o111, "native"),
    ] {
        let path = fixture.0.root.join(name);
        std::fs::write(&path, content).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        let plan = runner_core::file_plan(&cascade, &path, &["arg".into()]).unwrap();
        if expected_program == name {
            assert_eq!(plan.argv[0], path);
        } else {
            assert_eq!(plan.argv[0], expected_program);
        }
        assert_eq!(plan.argv.last().unwrap(), "arg");
    }
    let unsupported = fixture.0.root.join("unknown.txt");
    std::fs::write(&unsupported, "text").unwrap();
    assert!(runner_core::file_plan(&cascade, &unsupported, &[]).is_err());
    assert!(runner_core::file_plan(&cascade, &fixture.0.root.join("missing"), &[]).is_err());
}

#[test]
fn forced_node_refuses_jsx_without_falling_through_to_bun() {
    let fixture = Fixture::new();
    let policy = Policy {
        runtime: Some(runner_core::Choice {
            id: ProviderId::Node,
            from: runner_core::Layer::Cli,
        }),
        ..Policy::default()
    };
    let project = Project {
        present: vec![
            fixture.present(ProviderId::Node),
            fixture.present(ProviderId::Bun),
        ],
        ..Project::default()
    };
    let cascade = Cascade {
        tree: &fixture.0,
        project: &project,
        policy: &policy,
        registry: &REGISTRY,
        builtins: &[],
        dep: None,
        confirm: None,
    };
    for file in ["view.jsx", "view.tsx"] {
        let path = fixture.0.root.join(file);
        std::fs::write(&path, "const view = <div />").unwrap();
        let error = runner_core::file_plan(&cascade, &path, &[]).unwrap_err();
        assert!(matches!(
            error,
            Refusal::UnsupportedFile {
                provider: ProviderId::Node,
                ..
            }
        ));
    }
}

#[test]
#[cfg(windows)]
fn windows_file_plan_uses_posix_shell_paths_and_preserves_existing_interpreters() {
    let fixture = Fixture::new();
    let project = Project::default();
    let policy = Policy::default();
    let cascade = Cascade {
        tree: &fixture.0,
        project: &project,
        policy: &policy,
        registry: &REGISTRY,
        builtins: &[],
        dep: None,
        confirm: None,
    };
    let path = fixture.0.root.join("-script with spaces.sh");
    std::fs::write(&path, "#!/runner-test-nonexistent/bin/bash -e\n").unwrap();
    let plan = runner_core::file_plan(&cascade, &path, &["two words".into()]).unwrap();
    assert_eq!(
        plan.argv,
        ["bash", "-e", "./-script with spaces.sh", "two words"].map(std::ffi::OsString::from)
    );
    let interpreter = fixture.0.root.join("bash.exe");
    std::fs::write(&interpreter, "").unwrap();
    std::fs::write(
        &path,
        format!(
            "#!/usr/bin/env -S '{}' -e\n",
            interpreter.display().to_string().replace('\\', "/")
        ),
    )
    .unwrap();
    let plan = runner_core::file_plan(&cascade, &path, &[]).unwrap();
    assert_eq!(std::path::Path::new(&plan.argv[0]), interpreter);
}

#[test]
fn yarn_install_and_exec_use_the_same_observed_capabilities() {
    for (manifest, frozen_flag, exec_verb, deny_flag, deny_env, allow_env) in [
        (None, "--frozen-lockfile", "run", true, Some("false"), None),
        (
            Some("yarn@1.22.0"),
            "--frozen-lockfile",
            "run",
            true,
            None,
            None,
        ),
        (
            Some("yarn@4.1.0"),
            "--immutable",
            "exec",
            false,
            Some("false"),
            Some("true"),
        ),
    ] {
        let fixture = Fixture::new();
        if let Some(version) = manifest {
            std::fs::write(
                fixture.0.root.join("package.json"),
                format!(r#"{{"packageManager":"{version}"}}"#),
            )
            .unwrap();
        }
        std::fs::write(fixture.0.root.join("yarn.lock"), "").unwrap();
        let mut present = fixture.present(ProviderId::Yarn);
        let mut evidence = present.because.clone();
        runner_core::observe::derive(&fixture.0, &REGISTRY, &mut evidence).unwrap();
        present.because = evidence;
        let project = Project {
            present: vec![present],
            ..Project::default()
        };
        for (scripts, flag, env) in [
            (ScriptPolicy::Default, false, None),
            (ScriptPolicy::Deny, deny_flag, deny_env),
            (ScriptPolicy::Allow, false, allow_env),
        ] {
            let policy = Policy {
                frozen: true,
                scripts,
                ..Policy::default()
            };
            let plan = plan_with(
                &fixture.0,
                &project,
                &policy,
                &project.present[0],
                &Op::Install { operations: &[] },
                &REGISTRY,
            )
            .unwrap();
            assert!(plan.argv.iter().any(|arg| arg == frozen_flag));
            assert_eq!(plan.argv.iter().any(|arg| arg == "--ignore-scripts"), flag);
            assert_eq!(
                plan.env
                    .iter()
                    .find(|(key, _)| key == "YARN_ENABLE_SCRIPTS")
                    .map(|(_, value)| value.to_str().unwrap()),
                env
            );
            assert_ne!(plan.because.len(), 0);
            let exec = plan_with(
                &fixture.0,
                &project,
                &policy,
                &project.present[0],
                &Op::Exec {
                    name: "widget",
                    args: &[],
                },
                &REGISTRY,
            )
            .unwrap();
            assert_eq!(exec.argv[1], exec_verb);
        }
    }
}

#[test]
fn mise_frozen_install_follows_each_declared_config_lock_pair() {
    let pairs = REGISTRY
        .by_id(ProviderId::Mise)
        .caps
        .install
        .unwrap()
        .locked_only_with;
    assert_eq!(pairs.len(), 8);
    for (config, lock) in pairs {
        let fixture = Fixture::new();
        let config = fixture.0.root.join(config);
        let lock = fixture.0.root.join(lock);
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        let present = fixture.present(ProviderId::Mise);
        let policy = Policy {
            frozen: true,
            ..Policy::default()
        };
        let mut local = fixture.present(ProviderId::Npm);
        local.bin_dirs = vec![fixture.0.root.join("node_modules/.bin")];
        let project = Project {
            present: vec![local],
            ..Project::default()
        };
        let operations = ["install".into()];
        let planned = || {
            plan_with(
                &fixture.0,
                &project,
                &policy,
                &present,
                &Op::Install {
                    operations: &operations,
                },
                &REGISTRY,
            )
            .unwrap()
        };
        std::fs::write(&lock, "").unwrap();
        assert!(!planned().argv.iter().any(|arg| arg == "--locked"));
        std::fs::remove_file(&lock).unwrap();
        std::fs::write(&config, "").unwrap();
        assert!(!planned().argv.iter().any(|arg| arg == "--locked"));
        std::fs::write(&lock, "").unwrap();
        let plan = planned();
        assert_eq!(plan.argv, ["mise", "install", "--locked"]);
        assert_eq!(plan.trust, runner_core::Trust::Host);
        assert_eq!(plan.path_prepend.len(), 0);
    }
}

fn named_task(source: ProviderId, name: &str) -> runner_core::Task {
    runner_core::Task {
        name: name.into(),
        source,
        scope: Scope::Root,
        target: None,
        description: None,
        alias_of: None,
        forwards_to: None,
        detail: runner_core::TaskDetail::default(),
    }
}

#[test]
fn a_single_quiet_flag_clamps_stronger_requests_to_quiet() {
    let fixture = Fixture::new();
    let task = named_task(ProviderId::PackageJson, "build");
    let project = Project {
        present: vec![fixture.present(ProviderId::Npm)],
        tasks: vec![task.clone()],
        ..Project::default()
    };
    let policy = Policy {
        verbosity: runner_core::Verbosity::VeryQuiet,
        ..Policy::default()
    };
    let plan = plan_with(
        &fixture.0,
        &project,
        &policy,
        &project.present[0],
        &Op::Run {
            task: &task,
            args: &[],
        },
        &REGISTRY,
    )
    .unwrap();
    assert!(plan.argv.contains(&"--silent".into()), "{:?}", plan.argv);
    assert_eq!(
        plan.clamps
            .iter()
            .map(|clamp| (clamp.requested.as_str(), clamp.granted.as_str()))
            .collect::<Vec<_>>(),
        [("very-quiet", "quiet")]
    );
}

#[test]
fn a_chosen_provider_selects_its_own_sources_in_its_order() {
    let fixture = Fixture::new();
    let project = Project {
        present: vec![
            fixture.present(ProviderId::Deno),
            fixture.present(ProviderId::Bun),
        ],
        tasks: vec![
            named_task(ProviderId::PackageJson, "check"),
            named_task(ProviderId::Deno, "check"),
        ],
        ..Project::default()
    };
    let choice = runner_core::Choice {
        id: ProviderId::Deno,
        from: runner_core::Layer::Cli,
    };
    let mut by_pm = Policy::default();
    by_pm
        .pm
        .0
        .insert(runner_core::Ecosystem::Deno, choice.clone());
    let by_runtime = Policy {
        runtime: Some(choice),
        ..Policy::default()
    };
    let mut runtime_over_pm = by_runtime.clone();
    runtime_over_pm.pm.0.insert(
        runner_core::Ecosystem::Node,
        runner_core::Choice {
            id: ProviderId::Bun,
            from: runner_core::Layer::Cli,
        },
    );
    for policy in [&by_pm, &by_runtime, &runtime_over_pm] {
        let cascade = Cascade {
            tree: &fixture.0,
            project: &project,
            policy,
            registry: &REGISTRY,
            builtins: &[],
            dep: None,
            confirm: None,
        };
        let selected = runner_core::select(&cascade, "check").unwrap().unwrap();
        assert_eq!(selected.source, ProviderId::Deno, "{policy:?}");
    }
}

#[test]
fn a_configured_package_manager_keeps_per_task_pins() {
    let fixture = Fixture::new();
    let project = Project {
        present: vec![fixture.present(ProviderId::Npm)],
        tasks: vec![
            named_task(ProviderId::PackageJson, "build"),
            named_task(ProviderId::Just, "build"),
        ],
        ..Project::default()
    };
    let selected = |from: runner_core::Layer| {
        let mut policy = Policy::default();
        policy
            .task_sources
            .insert("build".into(), vec![ProviderId::Just]);
        policy.pm.0.insert(
            runner_core::Ecosystem::Node,
            runner_core::Choice {
                id: ProviderId::Npm,
                from,
            },
        );
        let cascade = Cascade {
            tree: &fixture.0,
            project: &project,
            policy: &policy,
            registry: &REGISTRY,
            builtins: &[],
            dep: None,
            confirm: None,
        };
        runner_core::select(&cascade, "build")
            .unwrap()
            .unwrap()
            .source
    };
    assert_eq!(
        selected(runner_core::Layer::ConfigFile("runner.toml".into())),
        ProviderId::Just
    );
    assert_eq!(selected(runner_core::Layer::Cli), ProviderId::PackageJson);
    assert_eq!(selected(runner_core::Layer::Env), ProviderId::PackageJson);
}

#[test]
fn a_forced_runtime_is_checked_in_the_tasks_member() {
    let fixture = Fixture::new();
    let member_dir = fixture.0.root.join("packages").join("web");
    std::fs::create_dir_all(&member_dir).unwrap();
    let member = Scope::Member {
        name: "web".into(),
        dir: member_dir,
    };
    let tree = Tree {
        cwd: fixture.0.root.clone(),
        root: fixture.0.root.clone(),
        members: vec![member.clone()],
    };
    let mut task = named_task(ProviderId::PackageJson, "build");
    task.scope = member.clone();
    let op = Op::Run {
        task: &task,
        args: &[],
    };
    let policy = Policy {
        runtime: Some(runner_core::Choice {
            id: ProviderId::Bun,
            from: runner_core::Layer::Cli,
        }),
        ..Policy::default()
    };
    let mut bun = fixture.present(ProviderId::Bun);
    bun.scope = member;
    let project = Project {
        present: vec![fixture.present(ProviderId::Npm), bun],
        ..Project::default()
    };
    let plan = runner_core::plan(&tree, &project, &policy, &op, &REGISTRY).unwrap();
    assert_eq!(plan.provider, Some(ProviderId::Bun));

    let rootless = Project {
        present: vec![fixture.present(ProviderId::Npm)],
        ..Project::default()
    };
    assert!(matches!(
        runner_core::plan(&tree, &rootless, &policy, &op, &REGISTRY),
        Err(Refusal::Invalid(message)) if message == "no evidence for runtime bun"
    ));
}

#[test]
fn node_task_version_boundary_is_checked_without_blocking_file_execution() {
    let fixture = Fixture::new();
    let task = named_task(ProviderId::PackageJson, "build");
    for (version, supported) in [
        ("v20.19.0", false),
        ("21.7.3", false),
        ("22.0.0", true),
        ("24.1.0", true),
    ] {
        let mut present = fixture.present(ProviderId::Node);
        present.version = Some(version.into());
        let project = Project {
            present: vec![present.clone()],
            ..Project::default()
        };
        let result = plan_with(
            &fixture.0,
            &project,
            &Policy::default(),
            &present,
            &Op::Run {
                task: &task,
                args: &[],
            },
            &REGISTRY,
        );
        assert_eq!(result.is_ok(), supported, "{version}: {result:?}");
        let file = fixture.0.root.join("script.js");
        std::fs::write(&file, "").unwrap();
        assert!(
            plan_with(
                &fixture.0,
                &project,
                &Policy::default(),
                &present,
                &Op::RunFile {
                    file: &file,
                    args: &[]
                },
                &REGISTRY
            )
            .is_ok()
        );
    }
}

#[test]
fn task_environment_uses_names_targets_and_source_aliases_consistently() {
    let fixture = Fixture::new();
    for (source, key, target) in [
        (ProviderId::Cargo, "root:cargo-alias#build", "test"),
        (ProviderId::Go, "root:go#build", "./cmd/build"),
    ] {
        let present = fixture.present(source);
        let project = Project {
            present: vec![present.clone()],
            ..Project::default()
        };
        let mut task = named_task(source, "build");
        task.target = Some(target.into());
        let mut policy = Policy::default();
        policy.env.task.insert(
            "build".into(),
            [
                ("BARE".into(), "yes".into()),
                ("WINNER".into(), "bare".into()),
            ]
            .into(),
        );
        policy
            .env
            .task
            .insert(key.into(), [("WINNER".into(), "qualified".into())].into());
        let plan = plan_with(
            &fixture.0,
            &project,
            &policy,
            &present,
            &Op::Run {
                task: &task,
                args: &[],
            },
            &REGISTRY,
        )
        .unwrap();
        assert!(plan.env.contains(&("BARE".into(), "yes".into())));
        assert!(plan.env.contains(&("WINNER".into(), "qualified".into())));
    }
}

#[test]
fn clean_includes_generated_python_metadata_and_preserves_files() {
    let fixture = Fixture::new();
    for name in ["build", "dist", "demo.egg-info", "keep"] {
        std::fs::create_dir(fixture.0.root.join(name)).unwrap();
    }
    std::fs::write(fixture.0.root.join("file.egg-info"), "retain").unwrap();
    let project = Project {
        present: vec![fixture.present(ProviderId::Python)],
        ..Project::default()
    };
    let plan = runner_core::clean::plan(&fixture.0, &project, &REGISTRY, false).unwrap();
    assert_eq!(plan.targets.len(), 3);
    runner_core::clean::execute(&plan).unwrap();
    assert!(!fixture.0.root.join("demo.egg-info").exists());
    assert!(fixture.0.root.join("keep").is_dir());
    assert!(fixture.0.root.join("file.egg-info").is_file());
}

#[test]
fn clean_from_a_member_includes_the_root_and_that_member() {
    let fixture = Fixture::new();
    let member_dir = fixture.0.root.join("packages").join("app");
    let other_dir = fixture.0.root.join("packages").join("other");
    for dir in [&fixture.0.root, &member_dir, &other_dir] {
        std::fs::create_dir_all(dir.join("node_modules")).unwrap();
    }
    let member = Scope::Member {
        name: "app".into(),
        dir: member_dir.clone(),
    };
    let other = Scope::Member {
        name: "other".into(),
        dir: other_dir,
    };
    let tree = Tree {
        cwd: member_dir.clone(),
        root: fixture.0.root.clone(),
        members: vec![member.clone(), other.clone()],
    };
    let mut local = fixture.present(ProviderId::Npm);
    local.scope = member;
    let mut elsewhere = fixture.present(ProviderId::Npm);
    elsewhere.scope = other;
    let project = Project {
        present: vec![fixture.present(ProviderId::Npm), local, elsewhere],
        ..Project::default()
    };
    let plan = runner_core::clean::plan(&tree, &project, &REGISTRY, false).unwrap();
    assert_eq!(
        plan.targets,
        [
            fixture.0.root.join("node_modules"),
            member_dir.join("node_modules"),
        ]
    );
}

#[test]
#[cfg(unix)]
fn declared_health_checks_report_findings_and_query_failures() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    let program = fixture.0.root.join("health-tool");
    std::fs::write(
        &program,
        "#!/bin/sh\nif [ \"$1\" = ls ]; then printf '%s' \
         '{\"node\":[{\"version\":\"22\",\"installed\":false}]}'; else echo broken >&2; exit 7; \
         fi\n",
    )
    .unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut providers = REGISTRY.0.to_vec();
    providers
        .iter_mut()
        .find(|provider| provider.id == ProviderId::Mise)
        .unwrap()
        .program = Some(Box::leak(
        program.to_str().unwrap().to_owned().into_boxed_str(),
    ));
    let registry = runner_core::Registry(Box::leak(providers.into_boxed_slice()));
    let present = fixture.present(ProviderId::Mise);
    let project = Project {
        present: vec![present.clone()],
        ..Project::default()
    };
    let policy = Policy::default();
    let health =
        runner_core::health::check(&fixture.0, &project, &policy, &present, 0, &registry).unwrap();
    assert_eq!(
        health,
        runner_core::Health::Problems(vec!["node@22 is declared but not installed".into()])
    );
    let error = runner_core::health::check(&fixture.0, &project, &policy, &present, 1, &registry)
        .unwrap_err();
    assert!(error.to_string().contains("broken"), "{error}");
    let plan = plan_with(
        &fixture.0,
        &project,
        &policy,
        &present,
        &Op::Health { check: 0 },
        &registry,
    )
    .unwrap();
    assert_eq!(plan.trust, runner_core::Trust::Host);
    assert_eq!(plan.path_prepend.len(), 0);
}

#[test]
fn an_activated_but_unconfigured_manager_takes_no_miss() {
    let fixture = Fixture::new();
    for (weight, manager) in [(Weight::Present, false), (Weight::Configured, true)] {
        let mut present = fixture.present(ProviderId::Mise);
        present.because[0].weight = weight;
        let project = Project {
            present: vec![present],
            ..Project::default()
        };
        let policy = Policy {
            reach: ReachPolicy::Allow,
            ..Policy::default()
        };
        let cascade = Cascade {
            tree: &fixture.0,
            project: &project,
            policy: &policy,
            registry: &REGISTRY,
            builtins: &[],
            dep: None,
            confirm: None,
        };
        let outcome = runner_core::dispatch(&cascade, "runner-audit-no-such-tool", &[]);
        assert_eq!(
            matches!(outcome, Ok((rung, _)) if rung.name == "manager"),
            manager,
            "{weight:?}"
        );
    }
}

#[test]
fn an_invocation_package_manager_without_exec_refuses_the_exec_rungs() {
    let fixture = Fixture::new();
    let project = Project {
        present: vec![
            fixture.present(ProviderId::Npm),
            fixture.present(ProviderId::Cargo),
        ],
        ..Project::default()
    };
    let outcome = |from: runner_core::Layer| {
        let mut policy = Policy {
            reach: ReachPolicy::Allow,
            ..Policy::default()
        };
        policy.pm.0.insert(
            runner_core::Ecosystem::Rust,
            runner_core::Choice {
                id: ProviderId::Cargo,
                from,
            },
        );
        let cascade = Cascade {
            tree: &fixture.0,
            project: &project,
            policy: &policy,
            registry: &REGISTRY,
            builtins: &[],
            dep: None,
            confirm: None,
        };
        runner_core::dispatch(&cascade, "runner-audit-no-such-tool", &[])
            .map(|(rung, dispatched)| (rung.name, dispatched))
    };
    for layer in [runner_core::Layer::Cli, runner_core::Layer::Env] {
        let refused = outcome(layer.clone());
        assert!(
            matches!(
                refused,
                Err(Refusal::NoCapability {
                    provider: ProviderId::Cargo,
                    op: "exec",
                })
            ),
            "{layer:?}: {refused:?}"
        );
    }
    let (rung, Dispatch::Plan(plan)) =
        outcome(runner_core::Layer::ConfigFile("runner.toml".into())).unwrap()
    else {
        panic!("a configured choice leaves exec to the other providers");
    };
    assert_eq!(rung, "exec");
    assert_eq!(plan.provider, Some(ProviderId::Npm));
}

#[test]
fn an_absent_invocation_package_manager_refuses_the_exec_rungs() {
    let fixture = Fixture::new();
    let project = Project {
        present: vec![fixture.present(ProviderId::Npm)],
        ..Project::default()
    };
    let mut policy = Policy {
        reach: ReachPolicy::Allow,
        ..Policy::default()
    };
    policy.pm.0.insert(
        runner_core::Ecosystem::Node,
        runner_core::Choice {
            id: ProviderId::Yarn,
            from: runner_core::Layer::Cli,
        },
    );
    let outcome = runner_core::dispatch(
        &cascade(&fixture, &project, &policy),
        "runner-audit-no-such-tool",
        &[],
    );
    assert!(
        matches!(&outcome, Err(Refusal::Invalid(message)) if message == "no evidence for package manager yarn"),
        "{outcome:?}"
    );
}

#[test]
fn a_runner_choice_refuses_another_runners_default_entry() {
    let fixture = Fixture::new();
    let project = Project {
        present: vec![
            fixture.present(ProviderId::Just),
            fixture.present(ProviderId::Make),
        ],
        ..Project::default()
    };
    let choose = |id: ProviderId| Policy {
        runner: Some(runner_core::Choice {
            id,
            from: runner_core::Layer::Cli,
        }),
        ..Policy::default()
    };
    let refused = runner_core::dispatch(
        &cascade(&fixture, &project, &choose(ProviderId::Just)),
        "make",
        &[],
    );
    assert!(
        matches!(
            &refused,
            Err(Refusal::NoRunnerTask {
                runner: ProviderId::Just,
                name,
            }) if name == "make"
        ),
        "{refused:?}"
    );
    let (rung, Dispatch::Plan(plan)) = runner_core::dispatch(
        &cascade(&fixture, &project, &choose(ProviderId::Make)),
        "make",
        &[],
    )
    .unwrap() else {
        panic!("the chosen runner's default entry plans");
    };
    assert_eq!(rung.name, "host");
    assert_eq!(plan.provider, Some(ProviderId::Make));
}

#[test]
fn an_invocation_package_manager_keeps_the_chosen_runtime_first() {
    let fixture = Fixture::new();
    let project = Project {
        present: vec![
            fixture.present(ProviderId::Npm),
            fixture.present(ProviderId::Bun),
        ],
        ..Project::default()
    };
    let mut policy = Policy {
        reach: ReachPolicy::Allow,
        runtime: Some(runner_core::Choice {
            id: ProviderId::Bun,
            from: runner_core::Layer::Cli,
        }),
        ..Policy::default()
    };
    policy.pm.0.insert(
        runner_core::Ecosystem::Node,
        runner_core::Choice {
            id: ProviderId::Npm,
            from: runner_core::Layer::Cli,
        },
    );
    let cascade = Cascade {
        tree: &fixture.0,
        project: &project,
        policy: &policy,
        registry: &REGISTRY,
        builtins: &[],
        dep: None,
        confirm: None,
    };
    let (rung, Dispatch::Plan(plan)) =
        runner_core::dispatch(&cascade, "runner-audit-no-such-tool", &[]).unwrap()
    else {
        panic!("the runtime executes the tool");
    };
    assert_eq!(rung.name, "exec");
    assert_eq!(plan.provider, Some(ProviderId::Bun));
}

#[test]
fn an_unreadable_task_source_stops_the_cascade_at_the_task_rung() {
    let fixture = Fixture::new();
    let project = Project {
        unread: vec![runner_core::Unread {
            provider: ProviderId::Just,
            scope: Scope::Root,
            message: "justfile: unexpected token".into(),
        }],
        ..Project::default()
    };
    let policy = Policy {
        reach: ReachPolicy::Allow,
        ..Policy::default()
    };
    let cascade = Cascade {
        tree: &fixture.0,
        project: &project,
        policy: &policy,
        registry: &REGISTRY,
        builtins: &["list"],
        dep: None,
        confirm: None,
    };
    let refusal = runner_core::dispatch(&cascade, "build", &[]).unwrap_err();
    assert!(
        matches!(&refusal, Refusal::Invalid(message) if message.contains("just tasks in root could not be read")),
        "{refusal:?}"
    );
    assert!(matches!(
        runner_core::dispatch(&cascade, "list", &[]),
        Ok((_, Dispatch::Builtin(_)))
    ));
}

#[test]
fn a_turbo_key_for_an_unknown_member_is_a_warning_beside_the_other_tasks() {
    let fixture = Fixture::new();
    std::fs::write(
        fixture.0.root.join("turbo.json"),
        r#"{"tasks":{"lint":{},"ghost#build":{}}}"#,
    )
    .unwrap();
    let present = fixture.present(ProviderId::Turbo);
    let found = (REGISTRY.by_id(ProviderId::Turbo).tasks.unwrap())(&present, &fixture.0).unwrap();
    assert_eq!(
        found
            .tasks
            .iter()
            .map(|task| task.name.as_str())
            .collect::<Vec<_>>(),
        ["lint"]
    );
    assert!(found.warnings[0].message.contains("ghost#build"));
}

#[test]
fn a_tool_only_pyproject_cleans_no_python_directories() {
    let fixture = Fixture::new();
    std::fs::write(
        fixture.0.root.join("pyproject.toml"),
        "[tool.ruff]\nline-length = 88\n",
    )
    .unwrap();
    std::fs::create_dir(fixture.0.root.join("build")).unwrap();
    let evidence = runner_core::observe::observe(&fixture.0, &REGISTRY).unwrap();
    let project =
        runner_core::resolve::resolve_presence(&fixture.0, evidence, &Policy::default(), &REGISTRY)
            .unwrap();
    assert!(
        project
            .present
            .iter()
            .all(|present| present.provider != ProviderId::Python)
    );
    let plan = runner_core::clean::plan(&fixture.0, &project, &REGISTRY, false).unwrap();
    assert!(plan.targets.is_empty(), "{:?}", plan.targets);

    std::fs::write(
        fixture.0.root.join("pyproject.toml"),
        "[project]\nname = \"demo\"\n",
    )
    .unwrap();
    let evidence = runner_core::observe::observe(&fixture.0, &REGISTRY).unwrap();
    let project =
        runner_core::resolve::resolve_presence(&fixture.0, evidence, &Policy::default(), &REGISTRY)
            .unwrap();
    let plan = runner_core::clean::plan(&fixture.0, &project, &REGISTRY, false).unwrap();
    assert_eq!(plan.targets.len(), 1, "{:?}", plan.targets);
}

#[test]
fn strict_policy_takes_no_package_manager_from_path() {
    let fixture = Fixture::new();
    std::fs::write(
        fixture.0.root.join("package.json"),
        r#"{"scripts":{"build":"echo"}}"#,
    )
    .unwrap();
    for (strict, synthesised) in [(false, true), (true, false)] {
        let evidence = runner_core::observe::observe(&fixture.0, &REGISTRY).unwrap();
        let policy = Policy {
            strict,
            ..Policy::default()
        };
        let project =
            runner_core::resolve::resolve_presence(&fixture.0, evidence, &policy, &REGISTRY)
                .unwrap();
        let has_manager = project
            .for_source(ProviderId::PackageJson, &Scope::Root, &policy, &REGISTRY)
            .is_some();
        if runner_core::probe_with("npm", &[]).is_some() {
            assert_eq!(has_manager, synthesised, "strict: {strict}");
        } else {
            assert!(!has_manager || !strict, "strict: {strict}");
        }
    }
}

#[test]
fn a_chosen_runtime_replaces_a_js_shebang_and_leaves_a_shell_script_alone() {
    let fixture = Fixture::new();
    let policy = Policy {
        runtime: Some(runner_core::Choice {
            id: ProviderId::Bun,
            from: runner_core::Layer::Cli,
        }),
        ..Policy::default()
    };
    let project = Project {
        present: vec![fixture.present(ProviderId::Bun)],
        ..Project::default()
    };
    let cascade = Cascade {
        tree: &fixture.0,
        project: &project,
        policy: &policy,
        registry: &REGISTRY,
        builtins: &[],
        dep: None,
        confirm: None,
    };
    for (shebang, program) in [("#!/usr/bin/env node\n", "bun"), ("#!/bin/sh\n", "/bin/sh")] {
        let path = fixture.0.root.join("tool");
        std::fs::write(&path, shebang).unwrap();
        let plan = runner_core::file_plan(&cascade, &path, &[]).unwrap();
        assert_eq!(plan.argv[0], program, "{shebang}");
    }
}

#[cfg(unix)]
fn executable(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[cfg(unix)]
#[test]
fn dev_engines_constraints_are_enforced_by_the_manifest_hook() {
    use runner_core::{Declared, OnFail};
    let fixture = Fixture::new();
    let bin = fixture.0.root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    executable(&bin.join("pnpm"));
    let task = named_task(ProviderId::PackageJson, "build");
    let op = Op::Run {
        task: &task,
        args: &[],
    };
    let declared = |version: &str, on_fail| {
        let mut present = fixture.present(ProviderId::Pnpm);
        present.version = Some("8.15.0".into());
        present.bin_dirs = vec![bin.clone()];
        present.because[0].weight = Weight::Declared;
        present.because[0].declared = Some(Declared::Constraint {
            version: Some(version.into()),
            on_fail,
        });
        present
    };
    let plan_for = |present: &Present| {
        let project = Project {
            present: vec![present.clone()],
            ..Project::default()
        };
        plan_with(
            &fixture.0,
            &project,
            &Policy::default(),
            present,
            &op,
            &REGISTRY,
        )
    };

    let refused = plan_for(&declared(">=9.0.0", OnFail::Error)).unwrap_err();
    let Refusal::Invalid(message) = refused else {
        panic!("{refused:?}");
    };
    for needle in ["pnpm", ">=9.0.0", "8.15.0", "onFail=error"] {
        assert!(message.contains(needle), "{message}");
    }

    let warned = plan_for(&declared(">=9.0.0", OnFail::Warn)).unwrap();
    assert_eq!(warned.warnings.len(), 1, "{:?}", warned.warnings);
    assert!(warned.warnings[0].message.contains("8.15.0"));

    let ignored = plan_for(&declared(">=9.0.0", OnFail::Ignore)).unwrap();
    assert_eq!(ignored.warnings.len(), 0);

    let satisfied = plan_for(&declared("^8.0.0", OnFail::Error)).unwrap();
    assert_eq!(satisfied.warnings.len(), 0);

    let unknown = plan_for(&declared("not-a-valid-range", OnFail::Error)).unwrap();
    assert_eq!(unknown.warnings.len(), 1, "{:?}", unknown.warnings);
    let message = &unknown.warnings[0].message;
    assert!(
        message.contains("cannot evaluate") && message.contains("not-a-valid-range"),
        "{message}"
    );

    let mut missing = declared(">=9.0.0", OnFail::Error);
    missing.bin_dirs = vec![];
    if runner_core::probe_with("pnpm", &[]).is_none() {
        let refused = plan_for(&missing).unwrap_err();
        assert!(
            refused.to_string().contains("not found on PATH"),
            "{refused}"
        );
    }
}

#[test]
fn a_manifest_that_disagrees_with_the_lockfile_is_recorded_and_refused_when_strict() {
    let fixture = Fixture::new();
    std::fs::write(
        fixture.0.root.join("package.json"),
        r#"{"packageManager":"yarn@4.0.0","scripts":{"build":"echo"}}"#,
    )
    .unwrap();
    std::fs::write(
        fixture.0.root.join("pnpm-lock.yaml"),
        "lockfileVersion: 9\n",
    )
    .unwrap();
    let task = named_task(ProviderId::PackageJson, "build");
    let op = Op::Run {
        task: &task,
        args: &[],
    };
    for on_mismatch in [OnMismatch::Proceed, OnMismatch::Refuse] {
        let evidence = runner_core::observe::observe(&fixture.0, &REGISTRY).unwrap();
        let policy = Policy {
            on_mismatch,
            ..Policy::default()
        };
        let project =
            runner_core::resolve::resolve_presence(&fixture.0, evidence, &policy, &REGISTRY)
                .unwrap();
        assert_eq!(
            project.disagreements.len(),
            1,
            "{:?}",
            project.disagreements
        );
        let disagreement = &project.disagreements[0];
        assert_eq!(disagreement.declared, ProviderId::Yarn);
        assert_eq!(disagreement.locked, ProviderId::Pnpm);
        assert!(disagreement.manifest.ends_with("package.json"));
        assert!(disagreement.lockfile.ends_with("pnpm-lock.yaml"));
        let chosen = project
            .for_source(ProviderId::PackageJson, &Scope::Root, &policy, &REGISTRY)
            .map(|present| present.provider);
        assert_eq!(chosen, Some(ProviderId::Yarn));
        let refuses = |op: &Op<'_>| {
            matches!(
                runner_core::plan(&fixture.0, &project, &policy, op, &REGISTRY),
                Err(Refusal::Mismatch(_))
            )
        };
        let expected = on_mismatch == OnMismatch::Refuse;
        assert_eq!(refuses(&op), expected, "{on_mismatch:?}: run");
        assert_eq!(
            refuses(&Op::ExecPackage {
                package: "typescript",
                bin: "tsc",
                args: &[],
            }),
            expected,
            "{on_mismatch:?}: exec-package"
        );
        assert_eq!(
            refuses(&Op::Exec {
                name: "tsc",
                args: &[],
            }),
            expected,
            "{on_mismatch:?}: exec"
        );
        let strict = Policy {
            strict: true,
            ..Policy::default()
        };
        let strict_project = runner_core::resolve::resolve_presence(
            &fixture.0,
            runner_core::observe::observe(&fixture.0, &REGISTRY).unwrap(),
            &strict,
            &REGISTRY,
        )
        .unwrap();
        assert!(
            runner_core::plan(&fixture.0, &strict_project, &strict, &op, &REGISTRY).is_ok(),
            "a strict fallback policy alone lets the manifest win"
        );
    }
    let evidence = runner_core::observe::observe(&fixture.0, &REGISTRY).unwrap();
    let mut policy = Policy {
        on_mismatch: OnMismatch::Refuse,
        ..Policy::default()
    };
    policy.pm.0.insert(
        runner_core::Ecosystem::Node,
        runner_core::Choice {
            id: ProviderId::Pnpm,
            from: runner_core::Layer::Cli,
        },
    );
    let project =
        runner_core::resolve::resolve_presence(&fixture.0, evidence, &policy, &REGISTRY).unwrap();
    let outcome = runner_core::plan(&fixture.0, &project, &policy, &op, &REGISTRY);
    assert!(
        !matches!(outcome, Err(Refusal::Mismatch(_))),
        "a chosen manager settles the disagreement: {outcome:?}"
    );
}

#[test]
fn an_invocation_package_manager_that_cannot_dispatch_the_source_is_refused() {
    let fixture = Fixture::new();
    let task = named_task(ProviderId::PackageJson, "build");
    let op = Op::Run {
        task: &task,
        args: &[],
    };
    let project = Project {
        present: vec![
            fixture.present(ProviderId::Npm),
            fixture.present(ProviderId::Cargo),
        ],
        ..Project::default()
    };
    let refuses = |from: runner_core::Layer| {
        let mut policy = Policy::default();
        policy.pm.0.insert(
            runner_core::Ecosystem::Rust,
            runner_core::Choice {
                id: ProviderId::Cargo,
                from,
            },
        );
        matches!(
            runner_core::plan(&fixture.0, &project, &policy, &op, &REGISTRY),
            Err(Refusal::NoCapability {
                provider: ProviderId::Cargo,
                ..
            })
        )
    };
    assert!(refuses(runner_core::Layer::Cli));
    assert!(refuses(runner_core::Layer::Env));
    assert!(!refuses(runner_core::Layer::ConfigFile(
        "runner.toml".into()
    )));
    let make = named_task(ProviderId::Make, "build");
    let mut policy = Policy::default();
    policy.pm.0.insert(
        runner_core::Ecosystem::Rust,
        runner_core::Choice {
            id: ProviderId::Cargo,
            from: runner_core::Layer::Cli,
        },
    );
    let outcome = runner_core::plan(
        &fixture.0,
        &Project {
            present: vec![
                fixture.present(ProviderId::Make),
                fixture.present(ProviderId::Cargo),
            ],
            ..Project::default()
        },
        &policy,
        &Op::Run {
            task: &make,
            args: &[],
        },
        &REGISTRY,
    );
    assert!(
        !matches!(outcome, Err(Refusal::NoCapability { .. })),
        "a source no manager dispatches ignores the choice: {outcome:?}"
    );
}

/// Observe and resolve `fixture` under `policy`, the way the CLI does.
fn resolved(fixture: &Fixture, policy: &Policy) -> Project {
    let evidence = runner_core::observe::observe(&fixture.0, &REGISTRY).unwrap();
    runner_core::resolve::resolve(&fixture.0, evidence, policy, &REGISTRY).unwrap()
}

fn manager_for_scripts(project: &Project, policy: &Policy) -> Option<ProviderId> {
    project
        .for_source(ProviderId::PackageJson, &Scope::Root, policy, &REGISTRY)
        .map(|present| present.provider)
}

fn cascade<'a>(fixture: &'a Fixture, project: &'a Project, policy: &'a Policy) -> Cascade<'a> {
    Cascade {
        tree: &fixture.0,
        project,
        policy,
        registry: &REGISTRY,
        builtins: &[],
        dep: None,
        confirm: None,
    }
}

fn words(plan: &runner_core::Plan) -> Vec<String> {
    plan.argv
        .iter()
        .map(|word| word.to_string_lossy().into_owned())
        .collect()
}

#[test]
fn an_empty_test_discovery_stops_the_cascade_before_the_host_path() {
    let fixture = Fixture::new();
    std::fs::write(
        fixture.0.root.join("package.json"),
        r#"{"scripts":{"build":"echo"}}"#,
    )
    .unwrap();
    let project = Project {
        present: vec![fixture.present(ProviderId::Npm)],
        ..Project::default()
    };
    let policy = Policy::default();
    let outcome = runner_core::dispatch(
        &cascade(&fixture, &project, &policy),
        "test",
        &["--watch".into()],
    );
    let Err(Refusal::NoTests { provider, dir, .. }) = outcome else {
        panic!("{outcome:?}");
    };
    assert_eq!(provider, ProviderId::Npm);
    assert_eq!(dir, fixture.0.root);
}

#[test]
fn a_legacy_package_manager_field_outranks_dev_engines_of_another_manager() {
    let fixture = Fixture::new();
    std::fs::write(
        fixture.0.root.join("package.json"),
        r#"{
  "packageManager": "bun@1.3.0",
  "devEngines": { "packageManager": { "name": "npm", "onFail": "ignore" } },
  "scripts": { "build": "echo ok" }
}"#,
    )
    .unwrap();
    let policy = Policy::default();
    let project = resolved(&fixture, &policy);
    assert_eq!(
        manager_for_scripts(&project, &policy),
        Some(ProviderId::Bun)
    );
    let task = project
        .tasks
        .iter()
        .find(|task| task.name == "build")
        .expect("build script");
    let plan = runner_core::plan(
        &fixture.0,
        &project,
        &policy,
        &Op::Run { task, args: &[] },
        &REGISTRY,
    )
    .unwrap();
    assert_eq!(plan.provider, Some(ProviderId::Bun));
    assert_eq!(words(&plan)[..2], ["bun", "run"]);
}

#[test]
fn a_leftover_yarnrc_cannot_override_a_pnpm_declaration() {
    let fixture = Fixture::new();
    std::fs::write(
        fixture.0.root.join("package.json"),
        r#"{"packageManager":"pnpm@9.0.0","scripts":{"build":"echo"}}"#,
    )
    .unwrap();
    std::fs::write(
        fixture.0.root.join(".yarnrc.yml"),
        "nodeLinker: node-modules\n",
    )
    .unwrap();
    let policy = Policy::default();
    let project = resolved(&fixture, &policy);
    assert_eq!(
        manager_for_scripts(&project, &policy),
        Some(ProviderId::Pnpm)
    );
    let yarn = project
        .present
        .iter()
        .filter(|present| present.provider == ProviderId::Yarn)
        .flat_map(|present| &present.because);
    for evidence in yarn {
        assert_ne!(evidence.weight, Weight::Declared, "{evidence:?}");
        assert_ne!(evidence.weight, Weight::Locked, "{evidence:?}");
    }
}

#[test]
fn json5_and_yaml_manifests_declare_the_package_manager() {
    for (name, body) in [
        (
            "package.json5",
            "{ packageManager: 'pnpm@9.0.0', scripts: { build: 'echo' } }",
        ),
        (
            "package.yaml",
            "packageManager: pnpm@9.0.0\nscripts:\n  build: echo\n",
        ),
    ] {
        let fixture = Fixture::new();
        std::fs::write(fixture.0.root.join(name), body).unwrap();
        let policy = Policy::default();
        let project = resolved(&fixture, &policy);
        assert_eq!(
            manager_for_scripts(&project, &policy),
            Some(ProviderId::Pnpm),
            "{name}"
        );
        assert!(
            project.tasks.iter().any(|task| task.name == "build"),
            "{name}: {:?}",
            project.tasks
        );
    }
}

#[test]
fn a_dev_engines_constraint_yields_to_a_stronger_declaration_or_a_policy_choice() {
    let task = named_task(ProviderId::PackageJson, "build");
    let op = Op::Run {
        task: &task,
        args: &[],
    };
    let plan_pnpm = |manifest: &str, policy: &Policy| {
        let fixture = Fixture::new();
        std::fs::write(fixture.0.root.join("package.json"), manifest).unwrap();
        let mut project = resolved(&fixture, policy);
        let present = project
            .present
            .iter_mut()
            .find(|present| present.provider == ProviderId::Pnpm)
            .expect("pnpm is declared");
        present.version = Some("9.0.0".into());
        let present = present.clone();
        plan_with(&fixture.0, &project, policy, &present, &op, &REGISTRY)
    };
    let both = r#"{
  "packageManager": "pnpm@9.0.0",
  "devEngines": { "packageManager": { "name": "pnpm", "version": ">=10", "onFail": "error" } },
  "scripts": { "build": "echo" }
}"#;
    let plan = plan_pnpm(both, &Policy::default()).unwrap();
    assert!(plan.warnings.is_empty(), "{:?}", plan.warnings);

    let dev_engines_only = r#"{
  "devEngines": { "packageManager": { "name": "pnpm", "version": ">=10", "onFail": "error" } },
  "scripts": { "build": "echo" }
}"#;
    let refused = plan_pnpm(dev_engines_only, &Policy::default()).unwrap_err();
    assert!(
        matches!(&refused, Refusal::Invalid(message) if message.contains(">=10")),
        "{refused:?}"
    );
    let mut chosen = Policy::default();
    chosen.pm.0.insert(
        runner_core::Ecosystem::Node,
        runner_core::Choice {
            id: ProviderId::Pnpm,
            from: runner_core::Layer::Cli,
        },
    );
    let plan = plan_pnpm(dev_engines_only, &chosen).unwrap();
    assert_eq!(plan.decided_by, [runner_core::Layer::Cli]);
}

#[test]
fn a_manager_that_cannot_take_the_operation_is_skipped_before_its_hook_runs() {
    let fixture = Fixture::new();
    std::fs::write(
        fixture.0.root.join("package.json"),
        r#"{"devEngines":{"packageManager":{"name":"pnpm","version":">=10","onFail":"error"}}}"#,
    )
    .unwrap();
    std::fs::write(fixture.0.root.join("Makefile"), "build:\n\t@echo ok\n").unwrap();
    std::fs::write(fixture.0.root.join("script.py"), "print(1)\n").unwrap();
    let policy = Policy::default();
    let mut project = resolved(&fixture, &policy);
    for present in &mut project.present {
        if present.provider == ProviderId::Pnpm {
            present.version = Some("9.0.0".into());
        }
    }
    let make = named_task(ProviderId::Make, "build");
    let plan = runner_core::plan(
        &fixture.0,
        &project,
        &policy,
        &Op::Run {
            task: &make,
            args: &[],
        },
        &REGISTRY,
    )
    .unwrap();
    assert_eq!(plan.provider, Some(ProviderId::Make));
    let script = fixture.0.root.join("script.py");
    let plan = runner_core::file_plan(&cascade(&fixture, &project, &policy), &script, &[]).unwrap();
    assert_ne!(plan.provider, Some(ProviderId::Pnpm));
}

#[test]
fn a_file_in_a_member_runs_with_the_member_bin_dirs_even_on_an_inherited_runtime() {
    let fixture = Fixture::new();
    let member_dir = fixture.0.root.join("packages").join("web");
    std::fs::create_dir_all(member_dir.join("node_modules").join(".bin")).unwrap();
    std::fs::create_dir_all(fixture.0.root.join("node_modules").join(".bin")).unwrap();
    let script = member_dir.join("script.js");
    std::fs::write(&script, "").unwrap();
    let member = Scope::Member {
        name: "web".into(),
        dir: member_dir.clone(),
    };
    let tree = Tree {
        cwd: member_dir.clone(),
        root: fixture.0.root.clone(),
        members: vec![member.clone()],
    };
    let mut node = fixture.present(ProviderId::Node);
    node.bin_dirs = vec![fixture.0.root.join("node_modules").join(".bin")];
    let mut npm = fixture.present(ProviderId::Npm);
    npm.scope = member.clone();
    npm.bin_dirs = vec![member_dir.join("node_modules").join(".bin")];
    let project = Project {
        present: vec![node.clone(), npm],
        ..Project::default()
    };
    let policy = Policy {
        runtime: Some(runner_core::Choice {
            id: ProviderId::Node,
            from: runner_core::Layer::Cli,
        }),
        ..Policy::default()
    };
    let plan = plan_with(
        &tree,
        &project,
        &policy,
        &node,
        &Op::RunFile {
            file: &script,
            args: &[],
        },
        &REGISTRY,
    )
    .unwrap();
    assert_eq!(plan.scope, member);
    assert_eq!(
        plan.path_prepend[0],
        member_dir.join("node_modules").join(".bin")
    );
    let found = plan_found(
        &tree,
        &project,
        &policy,
        member_dir.join("tool.sh"),
        &REGISTRY,
        &[],
    )
    .unwrap();
    assert_eq!(found.scope, member);
}

#[test]
fn deno_tasks_come_from_the_deno_config_even_when_the_manifest_names_deno() {
    let fixture = Fixture::new();
    std::fs::write(
        fixture.0.root.join("deno.json"),
        r#"{"tasks":{"build":"deno run build.ts"}}"#,
    )
    .unwrap();
    std::fs::write(
        fixture.0.root.join("package.json"),
        r#"{"packageManager":"deno@2.8.0"}"#,
    )
    .unwrap();
    let project = resolved(&fixture, &Policy::default());
    assert!(
        project
            .tasks
            .iter()
            .any(|task| task.name == "build" && task.source == ProviderId::Deno),
        "{:?} {:?}",
        project.tasks,
        project.unread
    );
}

#[test]
fn an_uppercase_justfile_is_observed() {
    let fixture = Fixture::new();
    std::fs::write(fixture.0.root.join("JUSTFILE"), "build:\n  echo build\n").unwrap();
    let project = resolved(&fixture, &Policy::default());
    let just = project
        .present
        .iter()
        .find(|present| present.provider == ProviderId::Just)
        .expect("just is present");
    assert!(just.because[0].at.ends_with("JUSTFILE"));
    if runner_core::probe_with("just", &[]).is_some() {
        assert!(project.tasks.iter().any(|task| task.name == "build"));
    }
}

#[test]
fn a_chosen_runtime_that_is_absent_refuses_instead_of_falling_through() {
    let fixture = Fixture::new();
    let script = fixture.0.root.join("script.js");
    std::fs::write(&script, "").unwrap();
    let project = Project {
        present: vec![fixture.present(ProviderId::Node)],
        ..Project::default()
    };
    let policy = Policy {
        runtime: Some(runner_core::Choice {
            id: ProviderId::Bun,
            from: runner_core::Layer::Cli,
        }),
        ..Policy::default()
    };
    let outcome = runner_core::file_plan(&cascade(&fixture, &project, &policy), &script, &[]);
    assert!(
        matches!(&outcome, Err(Refusal::Invalid(message)) if message.contains("bun")),
        "{outcome:?}"
    );
}

#[test]
fn a_lone_go_file_runs_through_go() {
    let fixture = Fixture::new();
    std::fs::write(fixture.0.root.join("hello.go"), "package main\n").unwrap();
    let project = Project::default();
    let policy = Policy::default();
    let (rung, Dispatch::Plan(plan)) =
        runner_core::dispatch(&cascade(&fixture, &project, &policy), "hello.go", &[]).unwrap()
    else {
        panic!("expected a file plan")
    };
    assert_eq!(rung.name, "file");
    assert_eq!(words(&plan)[..2], ["go", "run"]);
}

#[test]
fn deno_takes_a_registry_specifier_and_npm_does_not() {
    let fixture = Fixture::new();
    let op = Op::Exec {
        name: "jsr:@std/http/file-server",
        args: &["--help".into()],
    };
    let deno = fixture.present(ProviderId::Deno);
    let plan = plan_with(
        &fixture.0,
        &Project::default(),
        &Policy::default(),
        &deno,
        &op,
        &REGISTRY,
    )
    .unwrap();
    assert_eq!(
        words(&plan),
        ["deno", "x", "jsr:@std/http/file-server", "--help"]
    );
    let npm = fixture.present(ProviderId::Npm);
    assert!(matches!(
        plan_with(
            &fixture.0,
            &Project::default(),
            &Policy::default(),
            &npm,
            &op,
            &REGISTRY
        ),
        Err(Refusal::Unsafe(Unsafe::NameShape { .. }))
    ));
}

#[test]
fn a_default_runner_invocation_reads_the_runner_task_env() {
    let fixture = Fixture::new();
    let mut policy = Policy::default();
    policy
        .env
        .task
        .insert("make".into(), [("GREETING".into(), "hi".into())].into());
    let project = Project {
        present: vec![fixture.present(ProviderId::Make)],
        ..Project::default()
    };
    let (rung, Dispatch::Plan(plan)) =
        runner_core::dispatch(&cascade(&fixture, &project, &policy), "make", &[]).unwrap()
    else {
        panic!("expected the runner's own invocation")
    };
    assert_eq!(rung.name, "host");
    assert!(
        plan.env
            .iter()
            .any(|(key, value)| key == "GREETING" && value == "hi"),
        "{:?}",
        plan.env
    );
}

#[test]
fn bacon_keeps_default_job_arguments_behind_the_separator() {
    let fixture = Fixture::new();
    let plan = plan_with(
        &fixture.0,
        &Project::default(),
        &Policy::default(),
        &fixture.present(ProviderId::Bacon),
        &Op::RunDefault {
            args: &["--ignored".into()],
        },
        &REGISTRY,
    )
    .unwrap();
    assert_eq!(words(&plan), ["bacon", "--", "--ignored"]);
}

#[test]
fn discovered_typescript_tests_ask_node_to_strip_types() {
    let fixture = Fixture::new();
    std::fs::write(fixture.0.root.join("test.ts"), "").unwrap();
    let project = Project {
        present: vec![fixture.present(ProviderId::Pnpm)],
        ..Project::default()
    };
    let plan = runner_core::plan(
        &fixture.0,
        &project,
        &Policy {
            host_stderr: true,
            ..Policy::default()
        },
        &Op::Test { args: &[] },
        &REGISTRY,
    )
    .unwrap();
    assert_eq!(
        words(&plan),
        ["node", "--experimental-strip-types", "--test", "test.ts"]
    );
}

#[test]
fn node_test_discovery_refuses_jsx_and_tsx_files() {
    for file in ["test.jsx", "test.tsx", "view.test.jsx", "view.test.tsx"] {
        let fixture = Fixture::new();
        std::fs::write(fixture.0.root.join(file), "const view = <div />").unwrap();
        let project = Project {
            present: vec![fixture.present(ProviderId::Pnpm)],
            ..Project::default()
        };
        let outcome = runner_core::plan(
            &fixture.0,
            &project,
            &Policy::default(),
            &Op::Test { args: &[] },
            &REGISTRY,
        );
        assert!(
            matches!(outcome, Err(Refusal::NoTests { .. })),
            "{file}: {outcome:?}"
        );
        std::fs::write(fixture.0.root.join("test.ts"), "").unwrap();
        let plan = runner_core::plan(
            &fixture.0,
            &project,
            &Policy::default(),
            &Op::Test { args: &[] },
            &REGISTRY,
        )
        .unwrap();
        assert_eq!(
            words(&plan),
            ["node", "--experimental-strip-types", "--test", "test.ts"],
            "{file}"
        );
    }
}

#[test]
fn explicit_typescript_test_files_ask_node_to_strip_types() {
    let fixture = Fixture::new();
    let project = Project {
        present: vec![fixture.present(ProviderId::Pnpm)],
        ..Project::default()
    };
    let args = ["foo.test.ts".to_owned()];
    let plan = runner_core::plan(
        &fixture.0,
        &project,
        &Policy::default(),
        &Op::Test { args: &args },
        &REGISTRY,
    )
    .unwrap();
    assert_eq!(
        words(&plan),
        [
            "node",
            "--experimental-strip-types",
            "--test",
            "foo.test.ts"
        ]
    );
}

#[test]
fn the_stream_switch_reaches_only_the_owning_executable_once() {
    let fixture = Fixture::new();
    let pnpm = fixture.present(ProviderId::Pnpm);
    let project = Project {
        present: vec![pnpm.clone()],
        ..Project::default()
    };
    let policy = Policy {
        host_stderr: true,
        ..Policy::default()
    };
    let task = named_task(ProviderId::PackageJson, "build");
    let run = plan_with(
        &fixture.0,
        &project,
        &policy,
        &pnpm,
        &Op::Run {
            task: &task,
            args: &[],
        },
        &REGISTRY,
    )
    .unwrap();
    assert_eq!(words(&run), ["pnpm", "--use-stderr", "run", "build"]);
    let install = plan_with(
        &fixture.0,
        &project,
        &policy,
        &pnpm,
        &Op::Install { operations: &[] },
        &REGISTRY,
    )
    .unwrap();
    assert_eq!(words(&install), ["pnpm", "--use-stderr", "install"]);
}

#[test]
fn python_test_detection_looks_in_the_provider_scope() {
    let fixture = Fixture::new();
    std::fs::write(fixture.0.root.join("pytest.ini"), "[pytest]\n").unwrap();
    let src = fixture.0.root.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let tree = Tree {
        cwd: src,
        ..fixture.0.clone()
    };
    let uv = fixture.present(ProviderId::Uv);
    let plan = plan_with(
        &tree,
        &Project::default(),
        &Policy::default(),
        &uv,
        &Op::Test { args: &[] },
        &REGISTRY,
    )
    .unwrap();
    assert_eq!(words(&plan), ["uv", "run", "pytest"]);
}

#[test]
fn a_tracked_lockfile_outranks_an_untracked_one() {
    let fixture = Fixture::new();
    std::fs::write(
        fixture.0.root.join("package.json"),
        r#"{"scripts":{"build":"echo"}}"#,
    )
    .unwrap();
    std::fs::write(fixture.0.root.join("bun.lock"), "").unwrap();
    std::fs::write(fixture.0.root.join("package-lock.json"), "{}").unwrap();
    let tracked = |path: &std::path::Path| {
        path.file_name()
            .and_then(|name| name.to_str())
            .map(|name| name == "bun.lock")
    };
    let policy = Policy::default();
    let mut evidence = runner_core::observe::observe(&fixture.0, &REGISTRY).unwrap();
    runner_core::prefer_tracked_lockfiles(&mut evidence, &REGISTRY, &tracked);
    let project = runner_core::resolve::resolve(&fixture.0, evidence, &policy, &REGISTRY).unwrap();
    assert_eq!(
        manager_for_scripts(&project, &policy),
        Some(ProviderId::Bun)
    );
    assert_eq!(project.disagreements.len(), 0);
    let untouched = runner_core::observe::observe(&fixture.0, &REGISTRY).unwrap();
    let mut unanswered = untouched.clone();
    runner_core::prefer_tracked_lockfiles(&mut unanswered, &REGISTRY, &|_| None);
    assert_eq!(unanswered, untouched);
}

#[cfg(unix)]
#[test]
fn a_hoisted_binary_runs_in_the_invoking_member_scope() {
    use std::os::unix::fs::PermissionsExt as _;

    let fixture = Fixture::new();
    let member_dir = fixture.0.root.join("packages").join("app");
    let root_bin = fixture.0.root.join("node_modules").join(".bin");
    let member_bin = member_dir.join("node_modules").join(".bin");
    std::fs::create_dir_all(&root_bin).unwrap();
    std::fs::create_dir_all(&member_bin).unwrap();
    let tool = root_bin.join("tool");
    std::fs::write(&tool, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
    let member = Scope::Member {
        name: "app".into(),
        dir: member_dir.clone(),
    };
    let tree = Tree {
        cwd: member_dir,
        root: fixture.0.root.clone(),
        members: vec![member.clone()],
    };
    let mut root = fixture.present(ProviderId::Npm);
    root.bin_dirs = vec![root_bin.clone()];
    let mut local = fixture.present(ProviderId::Npm);
    local.scope = member.clone();
    local.bin_dirs = vec![member_bin.clone()];
    let project = Project {
        present: vec![root, local],
        ..Project::default()
    };
    let policy = Policy::default();
    let dep = |name: &str| -> Result<Option<std::path::PathBuf>, Refusal> {
        Ok((name == "tool").then(|| tool.clone()))
    };
    let mut cascade = Cascade {
        tree: &tree,
        project: &project,
        policy: &policy,
        registry: &REGISTRY,
        builtins: &[],
        dep: Some(&dep),
        confirm: None,
    };
    for expected_rung in ["dep", "bins"] {
        let (rung, Dispatch::Plan(plan)) = runner_core::dispatch(&cascade, "tool", &[]).unwrap()
        else {
            panic!("a hoisted binary plans");
        };
        assert_eq!(rung.name, expected_rung);
        assert_eq!(plan.found.as_deref(), Some(tool.as_path()));
        assert_eq!(plan.scope, member, "{expected_rung}");
        assert_eq!(
            plan.path_prepend.first(),
            Some(&member_bin),
            "{expected_rung}"
        );
        assert!(plan.path_prepend.contains(&root_bin), "{expected_rung}");
        cascade.dep = None;
    }
    let selected = runner_core::dependency_plan(&cascade, &tool, &[]).unwrap();
    assert_eq!(selected.scope, member);
    assert_eq!(selected.path_prepend.first(), Some(&member_bin));
}

#[test]
fn python_test_detection_looks_in_the_invocation_directory_too() {
    let fixture = Fixture::new();
    let tests = fixture.0.root.join("tests");
    std::fs::create_dir_all(&tests).unwrap();
    std::fs::write(tests.join("conftest.py"), "").unwrap();
    let tree = Tree {
        cwd: tests,
        ..fixture.0.clone()
    };
    let uv = fixture.present(ProviderId::Uv);
    let plan = plan_with(
        &tree,
        &Project::default(),
        &Policy::default(),
        &uv,
        &Op::Test { args: &[] },
        &REGISTRY,
    )
    .unwrap();
    assert_eq!(words(&plan), ["uv", "run", "pytest"]);
}

#[test]
fn an_explicit_runtime_choice_runs_a_file_despite_an_installer_mismatch() {
    let fixture = Fixture::new();
    std::fs::write(
        fixture.0.root.join("package.json"),
        r#"{"packageManager":"bun@1.3.0"}"#,
    )
    .unwrap();
    std::fs::write(
        fixture.0.root.join("pnpm-lock.yaml"),
        "lockfileVersion: 9\n",
    )
    .unwrap();
    let script = fixture.0.root.join("script.js");
    std::fs::write(&script, "").unwrap();
    let refuse = Policy {
        on_mismatch: OnMismatch::Refuse,
        ..Policy::default()
    };
    let project = resolved(&fixture, &refuse);
    assert_eq!(project.disagreements.len(), 1);
    let bun = project
        .present
        .iter()
        .find(|present| present.provider == ProviderId::Bun)
        .expect("the manifest declares bun");
    let run_file = Op::RunFile {
        file: &script,
        args: &[],
    };
    assert!(matches!(
        plan_with(&fixture.0, &project, &refuse, bun, &run_file, &REGISTRY),
        Err(Refusal::Mismatch(_))
    ));
    let chosen = Policy {
        runtime: Some(runner_core::Choice {
            id: ProviderId::Bun,
            from: runner_core::Layer::Cli,
        }),
        ..refuse
    };
    let plan = plan_with(&fixture.0, &project, &chosen, bun, &run_file, &REGISTRY).unwrap();
    assert_eq!(words(&plan)[0], "bun");
    assert!(matches!(
        plan_with(
            &fixture.0,
            &project,
            &chosen,
            bun,
            &Op::Install { operations: &[] },
            &REGISTRY
        ),
        Err(Refusal::Mismatch(_))
    ));
}

#[test]
fn an_unreadable_legacy_declaration_voids_dev_engines() {
    let fixture = Fixture::new();
    std::fs::write(
        fixture.0.root.join("package.json"),
        r#"{
  "packageManager": "pnpmm@9",
  "devEngines": { "packageManager": { "name": "yarn", "onFail": "ignore" } },
  "scripts": { "build": "echo ok" }
}"#,
    )
    .unwrap();
    let project = resolved(&fixture, &Policy::default());
    let declared: Vec<_> = project
        .present
        .iter()
        .flat_map(|present| &present.because)
        .filter(|evidence| evidence.weight == Weight::Declared)
        .collect();
    assert_eq!(declared, Vec::<&Evidence>::new());
}

#[test]
fn an_explicit_runner_choice_rejects_a_task_another_source_defines() {
    let fixture = Fixture::new();
    let project = Project {
        present: vec![
            fixture.present(ProviderId::Npm),
            fixture.present(ProviderId::Just),
        ],
        tasks: vec![
            named_task(ProviderId::PackageJson, "build"),
            named_task(ProviderId::PackageJson, "lint"),
            named_task(ProviderId::Just, "lint"),
        ],
        ..Project::default()
    };
    let policy = Policy {
        runner: Some(runner_core::Choice {
            id: ProviderId::Just,
            from: runner_core::Layer::Cli,
        }),
        ..Policy::default()
    };
    let cascade = cascade(&fixture, &project, &policy);
    assert_eq!(
        runner_core::select(&cascade, "build"),
        Err(Refusal::NoRunnerTask {
            runner: ProviderId::Just,
            name: "build".into(),
        })
    );
    assert_eq!(
        runner_core::select(&cascade, "lint")
            .unwrap()
            .map(|task| task.source),
        Some(ProviderId::Just)
    );
    assert_eq!(runner_core::select(&cascade, "tsc"), Ok(None));
    let outcome = runner_core::dispatch(&cascade, "build", &[]);
    assert!(
        matches!(outcome, Err(Refusal::NoRunnerTask { .. })),
        "{outcome:?}"
    );
}

#[test]
fn task_environment_keys_follow_the_documented_spellings() {
    let fixture = Fixture::new();
    let member_dir = fixture.0.root.join("rfc");
    std::fs::create_dir_all(&member_dir).unwrap();
    let member = Scope::Member {
        name: "rfc".into(),
        dir: member_dir,
    };
    let tree = Tree {
        members: vec![member.clone()],
        ..fixture.0.clone()
    };
    let mut present = fixture.present(ProviderId::Npm);
    present.scope = member.clone();
    let project = Project {
        present: vec![present.clone()],
        ..Project::default()
    };
    let mut task = named_task(ProviderId::PackageJson, "site");
    task.scope = member;
    let mut policy = Policy::default();
    for (key, value) in [
        ("site", "bare"),
        ("package.json:site", "source"),
        ("rfc:site", "member"),
        ("rfc:package.json#site", "fqn"),
        ("package.json#site", "undocumented"),
    ] {
        policy.env.task.insert(
            key.into(),
            [(
                key.replace([':', '#', '.'], "_").to_uppercase(),
                value.into(),
            )]
            .into(),
        );
    }
    let plan = plan_with(
        &tree,
        &project,
        &policy,
        &present,
        &Op::Run {
            task: &task,
            args: &[],
        },
        &REGISTRY,
    )
    .unwrap();
    let env: Vec<String> = plan
        .env
        .iter()
        .map(|(key, value)| format!("{}={}", key.to_string_lossy(), value.to_string_lossy()))
        .collect();
    assert_eq!(
        env,
        [
            "SITE=bare",
            "PACKAGE_JSON_SITE=source",
            "RFC_SITE=member",
            "RFC_PACKAGE_JSON_SITE=fqn",
        ]
    );
}
