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
