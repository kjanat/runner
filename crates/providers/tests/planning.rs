//! Regression tests for the architecture's planning contracts against real providers.

use std::sync::atomic::{AtomicUsize, Ordering};

use runner_core::{
    Cascade, Dispatch, Evidence, Op, Policy, Present, Project, ProviderId, ReachPolicy, Refusal,
    Scope, ScriptPolicy, SignalId, Tree, TrustPolicy, Unsafe, Weight, dispatch_from, plan_found,
    plan_with,
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
    let plan = plan_found(&fixture.0, &Project::default(), &policy, found.clone(), &[]).unwrap();
    assert_eq!(plan.because.len(), 1);
    assert_eq!(plan.because[0].at, found);
    assert_eq!(plan.because[0].provider, None);
    assert_eq!(plan.env, vec![("PROJECT_KEY".into(), "present".into())]);
    policy
        .env
        .project
        .insert("NODE_OPTIONS".into(), "--placeholder".into());
    assert!(matches!(
        plan_found(&fixture.0, &Project::default(), &policy, found.clone(), &[]),
        Err(Refusal::Unsafe(Unsafe::LoaderHook { .. }))
    ));
    policy.trust = TrustPolicy::Full;
    assert_eq!(
        plan_found(&fixture.0, &Project::default(), &policy, found, &[])
            .unwrap()
            .env
            .len(),
        2
    );
}

#[test]
fn frozen_tool_install_requires_its_declared_lockfile() {
    let fixture = Fixture::new();
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
fn unsupported_and_allowlist_script_policies_are_clamped() {
    let fixture = Fixture::new();
    for (id, scripts, clamped) in [
        (ProviderId::Bun, ScriptPolicy::Allow, true),
        (ProviderId::Cargo, ScriptPolicy::Deny, true),
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
        let project = runner_core::resolve(&fixture.0, evidence, &policy, &REGISTRY);
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
        assert!(project.present.is_empty());
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
