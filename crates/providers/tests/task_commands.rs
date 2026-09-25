//! Task argv regressions exercised through provider capabilities and core planning.
use runner_core::{Op, Policy, ProviderId, Scope, Signal, Task, TaskDetail, Tree, Verbosity};
use runner_providers::REGISTRY;

fn command(
    id: ProviderId,
    name: &str,
    args: &[String],
    verbosity: Verbosity,
) -> std::process::Command {
    let dir = tempfile::tempdir().unwrap();
    let provider = REGISTRY.by_id(id);
    let file = provider
        .signals
        .iter()
        .find_map(|s| match s {
            Signal::File(f) | Signal::FileUpwards(f) | Signal::Lockfile(f) => Some(f),
            _ => None,
        })
        .unwrap();
    let path = dir.path().join(file);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        if path
            .extension()
            .is_some_and(|e| e == "json" || e == "jsonc")
        {
            "{}"
        } else {
            ""
        },
    )
    .unwrap();
    let tree = Tree {
        cwd: dir.path().to_owned(),
        root: dir.path().to_owned(),
        members: vec![],
    };
    let policy = Policy {
        verbosity,
        ..Policy::default()
    };
    let evidence = runner_core::observe::observe(&tree, &REGISTRY).unwrap();
    let project =
        runner_core::resolve::resolve_presence(&tree, evidence, &policy, &REGISTRY).unwrap();
    let present = project.present_in(id, &Scope::Root).unwrap();
    let task = Task {
        name: name.into(),
        source: id,
        scope: Scope::Root,
        target: None,
        description: None,
        alias_of: None,
        forwards_to: None,
        detail: TaskDetail::default(),
    };
    let plan = runner_core::plan_with(
        &tree,
        &project,
        &policy,
        present,
        &Op::Run { task: &task, args },
        &REGISTRY,
    )
    .unwrap();
    runner_core::execute::command(&plan).unwrap()
}

fn argv(command: &std::process::Command) -> Vec<String> {
    command
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect()
}
#[test]
fn make_run_cmd_default_adds_no_verbosity_flag() {
    let v = Verbosity::Normal;
    assert_eq!(argv(&command(ProviderId::Make, "build", &[], v)), ["build"]);
}

#[test]
fn make_run_cmd_quiet_maps_to_host_flag() {
    let v = Verbosity::Quiet;
    assert_eq!(
        argv(&command(ProviderId::Make, "build", &[], v)),
        ["-s", "build"]
    );
}
#[test]
fn just_run_cmd_default_adds_no_verbosity_flag() {
    let v = Verbosity::Normal;
    assert_eq!(argv(&command(ProviderId::Just, "build", &[], v)), ["build"]);
}

#[test]
fn just_run_cmd_quiet_is_intentional_noop() {
    let v = Verbosity::Quiet;
    assert_eq!(argv(&command(ProviderId::Just, "build", &[], v)), ["build"]);
}
#[test]
fn go_task_run_cmd_default_adds_no_verbosity_flag() {
    let v = Verbosity::Normal;
    assert_eq!(argv(&command(ProviderId::Task, "build", &[], v)), ["build"]);
}

#[test]
fn go_task_run_cmd_quiet_maps_to_host_flag() {
    let v = Verbosity::Quiet;
    assert_eq!(
        argv(&command(ProviderId::Task, "build", &[], v)),
        ["-s", "build"]
    );
}
#[test]
fn bacon_run_cmd_omits_separator_when_no_args() {
    let cmd = command(ProviderId::Bacon, "check", &[], Verbosity::Normal);
    let argv: Vec<&std::ffi::OsStr> = cmd.get_args().collect();

    assert_eq!(argv, ["check"]);
}

#[test]
fn bacon_run_cmd_inserts_separator_before_forwarded_args() {
    // Bacon parses anything after the job name as its own options unless
    // separated by `--`. Without the separator, `--ignored` would error
    // out as an unknown bacon flag.
    let cmd = command(
        ProviderId::Bacon,
        "test",
        &["--ignored".into(), "my_test".into()],
        Verbosity::Normal,
    );
    let argv: Vec<&std::ffi::OsStr> = cmd.get_args().collect();

    assert_eq!(argv, ["test", "--", "--ignored", "my_test"]);
}
#[test]
fn mise_run_cmd_omits_separator_when_no_args() {
    let cmd = command(ProviderId::Mise, "build", &[], Verbosity::Normal);
    let argv: Vec<&std::ffi::OsStr> = cmd.get_args().collect();
    assert_eq!(argv, ["run", "build"]);
}

#[test]
fn mise_run_cmd_inserts_separator_before_forwarded_args() {
    let cmd = command(
        ProviderId::Mise,
        "test",
        &["--watch".into(), "unit".into()],
        Verbosity::Normal,
    );
    let argv: Vec<&std::ffi::OsStr> = cmd.get_args().collect();
    assert_eq!(argv, ["run", "test", "--", "--watch", "unit"]);
}

#[test]
fn mise_run_cmd_default_adds_no_verbosity_flag() {
    let v = Verbosity::Normal;
    assert_eq!(
        argv(&command(ProviderId::Mise, "build", &[], v)),
        ["run", "build"]
    );
}

#[test]
fn mise_run_cmd_quiet_maps_to_host_flag() {
    let v = Verbosity::Quiet;
    assert_eq!(
        argv(&command(ProviderId::Mise, "build", &[], v)),
        ["--quiet", "run", "build"]
    );
}
#[test]
fn turbo_run_cmd_default_adds_no_verbosity_flag() {
    let v = Verbosity::Normal;
    assert_eq!(
        argv(&command(ProviderId::Turbo, "build", &[], v)),
        ["run", "build"]
    );
}

#[test]
fn turbo_run_cmd_quiet_is_intentional_noop() {
    let v = Verbosity::Quiet;
    assert_eq!(
        argv(&command(ProviderId::Turbo, "build", &[], v)),
        ["run", "build"]
    );
}
#[test]
fn cargo_aliases_run_cmd_default_adds_no_verbosity_flag() {
    let v = Verbosity::Normal;
    assert_eq!(
        argv(&command(ProviderId::Cargo, "mytask", &[], v)),
        ["mytask"]
    );
}

#[test]
fn cargo_aliases_run_cmd_quiet_maps_to_host_flag() {
    let v = Verbosity::Quiet;
    assert_eq!(
        argv(&command(ProviderId::Cargo, "mytask", &[], v)),
        ["-q", "mytask"]
    );
}
#[test]
fn go_pm_run_cmd_uses_go_run_target() {
    let args = [String::from("--port"), String::from("3000")];
    let built: Vec<_> = command(ProviderId::Go, "./cmd/serve", &args, Verbosity::Normal)
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();

    assert_eq!(built, ["run", "./cmd/serve", "--port", "3000"]);
}
