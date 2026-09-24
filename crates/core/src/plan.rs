//! One command and everything that produced it.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::capability::{Discovery, NameShape, RunTaskCap, ScriptMechanism};
use crate::cascade::{CASCADE, Cap, Need, Rung};
use crate::env::project_may_set;
use crate::evidence::{Evidence, Present, Weight};
use crate::op::Op;
use crate::policy::{Choice, Layer, Policy, ScriptPolicy, TrustPolicy};
use crate::probe::{probe_in, probe_with};
use crate::provider::{Ecosystem, Kind, ProviderId};
use crate::reach::Reach;
use crate::registry::{Provider, Registry};
use crate::resolve::Project;
use crate::scope::Scope;
use crate::task::Task;
use crate::template::{Request, ScriptRequest, Template};
use crate::tree::Tree;
use crate::verbosity::Verbosity;

/// Whose `PATH` a plan runs with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Trust {
    /// The user's `PATH` only.
    Host,
    /// Project bin dirs first.
    Project,
}

/// A request the provider could not honour in full.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Clamp {
    /// What was asked for.
    pub requested: String,
    /// What the plan does instead.
    pub granted: String,
    /// The provider's reason.
    pub reason: &'static str,
}

/// One command, complete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// The provider that owns the command. `None` for a file on disk or a
    /// binary a search path holds.
    pub provider: Option<ProviderId>,
    /// The file or binary a rung found, when no provider owns the command.
    pub found: Option<PathBuf>,
    /// Program first.
    pub argv: Vec<OsString>,
    /// Working directory.
    pub cwd: PathBuf,
    /// Environment added to the inherited one.
    pub env: Vec<(OsString, OsString)>,
    /// Inherited variables removed from the child.
    pub env_remove: Vec<OsString>,
    /// Directories put before `PATH`. Empty when trust is `Host`.
    pub path_prepend: Vec<PathBuf>,
    /// Whose `PATH`.
    pub trust: Trust,
    /// Whether the command can fetch.
    pub reach: Reach,
    /// Requests the provider clamped.
    pub clamps: Vec<Clamp>,
    /// The evidence that produced the plan.
    pub because: Vec<Evidence>,
    /// The layers that decided it.
    pub decided_by: Vec<Layer>,
    /// The scope it runs in.
    pub scope: Scope,
}

impl Plan {
    /// The program, when the argv has one.
    #[must_use]
    pub fn program(&self) -> Option<&Path> {
        self.argv.first().map(Path::new)
    }
}

/// Why no plan was made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// An observation or input could not be interpreted safely.
    Invalid(String),
    /// Nothing on the cascade took the name.
    NotFound {
        /// The name.
        name: String,
        /// The rungs tried, in order.
        tried: Vec<Rung>,
    },
    /// A network rung was refused by policy or the user.
    Declined {
        /// The name.
        name: String,
        /// The rung that would have fetched.
        rung: Rung,
    },
    /// The provider has no capability for the op.
    NoCapability {
        /// The provider.
        provider: ProviderId,
        /// The op.
        op: &'static str,
    },
    /// More than one provider could take the request.
    Ambiguous {
        /// Every candidate and its scope.
        candidates: Vec<(ProviderId, Scope)>,
    },
    /// The request would cross a trust boundary.
    Unsafe(Unsafe),
}

/// A trust boundary a request would cross.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unsafe {
    /// A project-trust env layer sets a loader hook.
    LoaderHook {
        /// The variable.
        name: String,
    },
    /// A name of the wrong shape for the exec primitive.
    NameShape {
        /// The name.
        name: String,
        /// The provider.
        provider: ProviderId,
    },
    /// A path that resolves outside the root.
    EscapesRoot {
        /// The path.
        path: PathBuf,
    },
}

/// Directories test discovery never descends into.
pub const SKIPPED_DIRS: &[&str] = &[
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

impl NameShape {
    /// The shape of `name`. A scoped package name counts as bare.
    #[must_use]
    pub fn of(name: &str) -> Self {
        let rest = name
            .strip_prefix('@')
            .and_then(|scoped| scoped.split_once('/'))
            .map_or(name, |(_, rest)| rest);
        if rest.contains(['/', '\\']) || rest.starts_with('.') {
            Self::PATH_LIKE
        } else if rest.contains('@') {
            Self::VERSIONED
        } else {
            Self::BARE
        }
    }
}

/// Turn one request into one command, choosing the provider.
///
/// The provider is the policy's runtime choice when it can do the op, then
/// the policy's package-manager choice, then each present provider that can,
/// in the project's own order.
///
/// # Errors
///
/// `NoCapability` when no present provider can do the op, `Unsafe` when the
/// request crosses a trust boundary.
pub fn plan(
    tree: &Tree,
    project: &Project,
    policy: &Policy,
    op: &Op<'_>,
    registry: &Registry,
) -> Result<Plan, Refusal> {
    let mut last = None;
    for present in candidates(project, policy, op, registry) {
        match plan_with(tree, project, policy, present, op, registry) {
            Ok(made) => return Ok(made),
            Err(refusal @ Refusal::Unsafe(_)) => return Err(refusal),
            Err(refusal)
                if matches!(op, Op::Test { .. })
                    && policy
                        .runtime
                        .as_ref()
                        .is_some_and(|choice| choice.id == present.provider) =>
            {
                return Err(refusal);
            }
            Err(refusal) => last = Some(refusal),
        }
    }
    Err(last.unwrap_or_else(|| Refusal::NoCapability {
        provider: project
            .present
            .first()
            .map_or(ProviderId::ALL[0], |present| present.provider),
        op: op.name(),
    }))
}

/// The present providers that may take `op`, policy choices first.
fn candidates<'a>(
    project: &'a Project,
    policy: &Policy,
    op: &Op<'_>,
    registry: &Registry,
) -> Vec<&'a Present> {
    let mut ordered: Vec<&Present> = Vec::new();
    let mut push = |present: &'a Present| {
        if !ordered.iter().any(|seen| std::ptr::eq(*seen, present)) {
            ordered.push(present);
        }
    };
    let by_choice = |choice: &Choice| {
        project
            .present
            .iter()
            .find(|present| present.provider == choice.id)
    };
    if let Some(present) = policy.runtime.as_ref().and_then(by_choice) {
        push(present);
    }
    for choice in policy.pm.0.values() {
        if let Some(present) = by_choice(choice) {
            push(present);
        }
    }
    for present in &project.present {
        let provider = registry.by_id(present.provider);
        let manager = provider.kind.contains(Kind::TOOL_MANAGER);
        if (provider.kind == Kind::RUNTIME && !matches!(op, Op::RunFile { .. }))
            || (manager && matches!(op, Op::Exec { .. }))
        {
            continue;
        }
        push(present);
    }
    ordered
}

/// One op's program, argv template and the terms the command runs under.
struct Shape<'a> {
    program: Option<&'static str>,
    template: Template,
    reach: Reach,
    trust: Trust,
    cwd: PathBuf,
    task: Option<&'a str>,
}

/// The values a render fills the template with, owned so the caller can
/// borrow the discovered files back into the request.
struct Fill<'a> {
    request: Request<'a>,
    files: Vec<PathBuf>,
}

/// Everything shaping an op reads.
struct Shaping<'c, 'a> {
    tree: &'c Tree,
    policy: &'c Policy,
    provider: &'c Provider,
    present: &'c Present,
    chosen_as_runtime: bool,
    op: Op<'a>,
}

impl<'a> Shaping<'_, 'a> {
    const fn refuse(&self) -> Refusal {
        Refusal::NoCapability {
            provider: self.provider.id,
            op: self.op.name(),
        }
    }

    /// Whether the op runs as the project or as the user.
    const fn trust(&self) -> Trust {
        if self.provider.kind.contains(Kind::TOOL_MANAGER) {
            Trust::Host
        } else {
            Trust::Project
        }
    }

    fn shape(&self, fill: &mut Fill<'a>) -> Result<Shape<'a>, Refusal> {
        match self.op {
            Op::Run { task, args } => self.run(fill, task, args),
            Op::RunDefault { args } => {
                fill.request.args = args;
                Ok(Shape {
                    program: self.provider.program,
                    template: self
                        .provider
                        .caps
                        .run_default
                        .ok_or_else(|| self.refuse())?,
                    reach: Reach::Local,
                    trust: Trust::Project,
                    cwd: scope_dir(self.tree, &self.present.scope),
                    task: Some(self.provider.label),
                })
            }
            Op::Exec { name, args } => self.exec(fill, name, args),
            Op::ExecPackage { package, bin, args } => {
                let cap = self
                    .provider
                    .caps
                    .package_exec
                    .ok_or_else(|| self.refuse())?;
                fill.request.package = Some(package);
                fill.request.name = Some(bin);
                fill.request.args = args;
                let template = self
                    .provider
                    .caps
                    .as_runtime
                    .and_then(|cap| cap.package_exec)
                    .filter(|_| self.chosen_as_runtime)
                    .unwrap_or(cap.argv);
                Ok(Shape {
                    program: cap.program.or(self.provider.program),
                    template,
                    reach: cap.reach,
                    trust: self.trust(),
                    cwd: self.tree.cwd.clone(),
                    task: None,
                })
            }
            Op::RunFile { file, args } => self.run_file(fill, file, args),
            Op::Test { args } => self.test(fill, args),
            Op::Install { operations } => self.install(fill, operations),
            Op::Health => self.health(),
            Op::Clean => Err(self.refuse()),
        }
    }

    fn run(
        &self,
        fill: &mut Fill<'a>,
        task: &'a Task,
        args: &'a [String],
    ) -> Result<Shape<'a>, Refusal> {
        let template = run_task_template(self.provider, task.source, self.chosen_as_runtime)
            .ok_or_else(|| self.refuse())?;
        fill.request.task = Some(task.target.as_deref().unwrap_or(task.name.as_str()));
        fill.request.args = args;
        Ok(Shape {
            program: self.provider.program,
            template,
            reach: Reach::Local,
            trust: Trust::Project,
            cwd: scope_dir(self.tree, &task.scope),
            task: Some(task.name.as_str()),
        })
    }

    fn exec(
        &self,
        fill: &mut Fill<'a>,
        name: &'a str,
        args: &'a [String],
    ) -> Result<Shape<'a>, Refusal> {
        let cap = self.provider.caps.exec.ok_or_else(|| self.refuse())?;
        if !cap.accepts.contains(NameShape::of(name)) {
            return Err(Refusal::Unsafe(Unsafe::NameShape {
                name: name.to_owned(),
                provider: self.provider.id,
            }));
        }
        fill.request.name = Some(name);
        fill.request.args = args;
        Ok(Shape {
            program: cap.program.or(self.provider.program),
            template: self
                .provider
                .caps
                .as_runtime
                .and_then(|runtime| runtime.exec)
                .filter(|_| self.chosen_as_runtime)
                .unwrap_or(cap.argv),
            reach: cap.reach,
            trust: self.trust(),
            cwd: self.tree.cwd.clone(),
            task: None,
        })
    }

    fn run_file(
        &self,
        fill: &mut Fill<'a>,
        source: &'a Path,
        args: &'a [String],
    ) -> Result<Shape<'a>, Refusal> {
        let cap = self.provider.caps.run_file.ok_or_else(|| self.refuse())?;
        let runs = source
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| cap.extensions.contains(&ext));
        let interpreted = self.chosen_as_runtime
            && read_shebang(source).is_some_and(|s| {
                Path::new(&s.program)
                    .file_name()
                    .and_then(|p| p.to_str())
                    .is_some_and(|name| self.provider.caps.file_interpreters.contains(&name))
            });
        if !runs && !interpreted {
            return Err(self.refuse());
        }
        fill.request.file = Some(source);
        fill.request.args = args;
        Ok(Shape {
            program: cap.program.or(self.provider.program),
            template: cap.argv,
            reach: Reach::Local,
            trust: Trust::Project,
            cwd: self.tree.cwd.clone(),
            task: None,
        })
    }

    fn test(&self, fill: &mut Fill<'a>, args: &'a [String]) -> Result<Shape<'a>, Refusal> {
        let cap = self.provider.caps.test.ok_or_else(|| self.refuse())?;
        let template = match cap.discovery {
            Discovery::Tool => cap.argv,
            Discovery::Files(patterns) => {
                if !args.iter().any(|arg| !arg.starts_with('-')) {
                    fill.files = discover(&self.tree.cwd, patterns);
                    if fill.files.is_empty() {
                        return Err(self.refuse());
                    }
                }
                cap.argv
            }
            Discovery::Detect(detect) => detect(&self.tree.cwd).ok_or_else(|| self.refuse())?,
        };
        fill.request.args = args;
        Ok(Shape {
            program: cap.program.or(self.provider.program),
            template,
            reach: Reach::Local,
            trust: Trust::Project,
            cwd: self.tree.cwd.clone(),
            task: Some("test"),
        })
    }

    fn install(&self, fill: &mut Fill<'a>, operations: &'a [String]) -> Result<Shape<'a>, Refusal> {
        let cap = self.provider.caps.install.ok_or_else(|| self.refuse())?;
        fill.request.op = operations.first().map(String::as_str);
        fill.request.frozen = (self.policy.frozen
            && cap.locked_only_with.is_none_or(|file| {
                scope_dir(self.tree, &self.present.scope)
                    .join(file)
                    .is_file()
            }))
        .then_some(cap.frozen);
        fill.request.scripts = Some((
            cap.scripts,
            match self.policy.scripts {
                ScriptPolicy::Default => ScriptRequest::Default,
                ScriptPolicy::Deny => ScriptRequest::Deny,
                ScriptPolicy::Allow => ScriptRequest::Allow,
            },
        ));
        Ok(Shape {
            program: self.provider.program,
            template: cap.argv,
            reach: Reach::Network,
            trust: self.trust(),
            cwd: scope_dir(self.tree, &self.present.scope),
            task: None,
        })
    }

    fn health(&self) -> Result<Shape<'a>, Refusal> {
        let cap = self.provider.caps.health.ok_or_else(|| self.refuse())?;
        Ok(Shape {
            program: self.provider.program,
            template: cap.argv,
            reach: Reach::Local,
            trust: Trust::Host,
            cwd: scope_dir(self.tree, &self.present.scope),
            task: None,
        })
    }
}

/// Turn one request into one command through `present`.
///
/// # Errors
///
/// `NoCapability` when the provider lacks the capability or a test runner
/// finds nothing to run, `Unsafe` when the name has a shape the exec
/// primitive does not take or an env layer sets a loader hook.
pub fn plan_with(
    tree: &Tree,
    project: &Project,
    policy: &Policy,
    present: &Present,
    op: &Op<'_>,
    registry: &Registry,
) -> Result<Plan, Refusal> {
    let provider = registry.by_id(present.provider).for_present(present);
    let quiet = provider.caps.quiet;
    let mut clamps = Vec::new();
    if policy.verbosity != Verbosity::Normal && policy.verbosity.index() > quiet.strongest() {
        clamps.push(Clamp {
            requested: policy.verbosity.label().to_owned(),
            granted: Verbosity::ALL[quiet.strongest()].label().to_owned(),
            reason: quiet.limitation,
        });
    }
    if matches!(op, Op::Install { .. })
        && let Some(cap) = provider.caps.install
    {
        let mechanism = match policy.scripts {
            ScriptPolicy::Default => None,
            ScriptPolicy::Deny => Some(("deny", cap.scripts.deny)),
            ScriptPolicy::Allow => Some(("allow", cap.scripts.allow)),
        };
        if let Some((requested, mechanism)) = mechanism {
            let reason = match mechanism {
                ScriptMechanism::Unsupported => Some("provider cannot enforce this script policy"),
                ScriptMechanism::Warn(reason) => Some(reason),
                _ => None,
            };
            if let Some(reason) = reason {
                clamps.push(Clamp {
                    requested: format!("scripts={requested}"),
                    granted: "provider script defaults".into(),
                    reason,
                });
            }
        }
    }
    let mut fill = Fill {
        request: Request {
            quiet: quiet.at(policy.verbosity.index()),
            stream: policy.host_stderr.then_some(quiet.stream).flatten(),
            ..Request::default()
        },
        files: Vec::new(),
    };
    let shaping = Shaping {
        tree,
        policy,
        provider: &provider,
        present,
        chosen_as_runtime: policy
            .runtime
            .as_ref()
            .is_some_and(|choice| choice.id == provider.id),
        op: *op,
    };
    let shape = shaping.shape(&mut fill)?;
    let Fill { mut request, files } = fill;
    request.files = &files;
    let program = shape.program.ok_or_else(|| shaping.refuse())?;
    let rendered = shape.template.render(&request);
    let mut argv = Vec::with_capacity(rendered.args.len() + 1);
    argv.push(OsString::from(program));
    if policy.host_stderr
        && let Some(stream) = quiet.stream
    {
        argv.extend(stream.render(&Request::default()).args);
    }
    argv.extend(rendered.args);
    let mut env = rendered.env;
    env.extend(env_layers(policy, Some(provider.id), shape.task)?);
    let scope = match op {
        Op::Run { task, .. } => task.scope.clone(),
        _ => present.scope.clone(),
    };
    let path_prepend = match shape.trust {
        Trust::Host => Vec::new(),
        Trust::Project => bin_dirs(project, &scope),
    };
    Ok(Plan {
        provider: Some(provider.id),
        found: None,
        argv,
        cwd: shape.cwd,
        env,
        env_remove: Vec::new(),
        path_prepend,
        trust: shape.trust,
        reach: shape.reach,
        clamps,
        because: present.because.clone(),
        decided_by: decided_by(policy, present),
        scope,
    })
}

/// A plan for a file or binary a rung found, with no provider behind it.
///
/// # Errors
/// Refuses forbidden project environment variables.
pub fn plan_found(
    tree: &Tree,
    project: &Project,
    policy: &Policy,
    found: PathBuf,
    args: &[String],
) -> Result<Plan, Refusal> {
    let mut words = Vec::with_capacity(args.len() + 1);
    words.push(OsString::from(&found));
    words.extend(args.iter().map(OsString::from));
    plan_argv(tree, project, policy, found, words)
}

/// A plan for an argv a rung built around a file it found.
///
/// # Errors
/// Refuses forbidden project environment variables.
pub fn plan_argv(
    tree: &Tree,
    project: &Project,
    policy: &Policy,
    found: PathBuf,
    argv: Vec<OsString>,
) -> Result<Plan, Refusal> {
    let scope = tree
        .members
        .iter()
        .filter_map(|scope| match scope {
            Scope::Member { dir, .. } if tree.cwd.starts_with(dir) => {
                Some((dir.components().count(), scope))
            }
            _ => None,
        })
        .max_by_key(|(depth, _)| *depth)
        .map_or(Scope::Root, |(_, scope)| scope.clone());
    let evidence = Evidence {
        provider: None,
        signal: None,
        at: found.clone(),
        scope: scope.clone(),
        weight: Weight::Present,
        declared: None,
    };
    Ok(Plan {
        provider: None,
        found: Some(found),
        argv,
        cwd: tree.cwd.clone(),
        env: env_layers(policy, None, None)?,
        env_remove: Vec::new(),
        path_prepend: bin_dirs(project, &scope),
        trust: Trust::Project,
        reach: Reach::Local,
        clamps: Vec::new(),
        because: vec![evidence],
        decided_by: Vec::new(),
        scope,
    })
}

/// The run-task template `provider` uses for `source`, the runtime form when
/// policy named the provider as the runtime.
fn run_task_template(
    provider: &Provider,
    source: ProviderId,
    as_runtime: bool,
) -> Option<Template> {
    let RunTaskCap { argv, sources } = provider.caps.run_task?;
    if !sources.contains(&source) {
        return None;
    }
    Some(
        provider
            .caps
            .as_runtime
            .and_then(|runtime| runtime.run_task)
            .filter(|_| as_runtime)
            .unwrap_or(argv),
    )
}

/// The directory a scope lives in.
#[must_use]
pub fn scope_dir(tree: &Tree, scope: &Scope) -> PathBuf {
    match scope {
        Scope::Root => tree.root.clone(),
        Scope::Member { dir, .. } => dir.clone(),
    }
}

/// Every present provider's bin dirs, the plan's own scope first.
fn bin_dirs(project: &Project, scope: &Scope) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    let ordered = project
        .present
        .iter()
        .filter(|present| present.scope == *scope)
        .chain(
            project
                .present
                .iter()
                .filter(|present| present.scope != *scope),
        );
    for present in ordered {
        for dir in &present.bin_dirs {
            if !dirs.contains(dir) {
                dirs.push(dir.clone());
            }
        }
    }
    dirs
}

/// The project, tool and task env layers, narrowest last, filtered by trust.
fn env_layers(
    policy: &Policy,
    provider: Option<ProviderId>,
    task: Option<&str>,
) -> Result<Vec<(OsString, OsString)>, Refusal> {
    let layers = [
        Some(&policy.env.project),
        provider.and_then(|provider| policy.env.tool.get(&provider)),
        task.and_then(|task| policy.env.task.get(task)),
    ];
    let mut out: Vec<(OsString, OsString)> = Vec::new();
    for table in layers.into_iter().flatten() {
        for (key, value) in table {
            if policy.trust == TrustPolicy::Project && !project_may_set(key) {
                return Err(Refusal::Unsafe(Unsafe::LoaderHook { name: key.clone() }));
            }
            out.retain(|(seen, _)| seen != key.as_str());
            out.push((OsString::from(key), OsString::from(value)));
        }
    }
    Ok(out)
}

/// The layer that chose the provider, else the layer its strongest evidence
/// stands for.
fn decided_by(policy: &Policy, present: &Present) -> Vec<Layer> {
    let choice = policy
        .runtime
        .iter()
        .chain(policy.pm.0.values())
        .chain(policy.runner.iter())
        .find(|choice| choice.id == present.provider)
        .map(|choice| choice.from.clone());
    if let Some(layer) = choice {
        return vec![layer];
    }
    present
        .because
        .first()
        .map(|evidence| match evidence.weight {
            Weight::Locked => Layer::Lockfile(evidence.at.clone()),
            Weight::Probed => Layer::Probe,
            Weight::Declared | Weight::Configured | Weight::Present => {
                Layer::Manifest(evidence.at.clone())
            }
        })
        .into_iter()
        .collect()
}

/// Every file under `root` matching one of `patterns`, sorted, relative to
/// `root`. A pattern is a file name, or `*` followed by a suffix.
#[must_use]
pub fn discover(root: &Path, patterns: &[&str]) -> Vec<PathBuf> {
    let mut found = Vec::new();
    walk(root, root, patterns, &mut found);
    found.sort();
    found
}

fn walk(root: &Path, dir: &Path, patterns: &[&str], found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if path.is_dir() {
            if !SKIPPED_DIRS.contains(&name) {
                walk(root, &path, patterns, found);
            }
        } else if patterns.iter().any(|pattern| matches(pattern, name)) {
            found.push(path.strip_prefix(root).unwrap_or(&path).to_path_buf());
        }
    }
}

fn matches(pattern: &str, name: &str) -> bool {
    pattern.strip_prefix('*').map_or_else(
        || pattern == name,
        |suffix| name.len() > suffix.len() && name.ends_with(suffix),
    )
}

/// The ecosystem an op belongs to, when it names one.
#[must_use]
pub fn ecosystem_of(op: &Op<'_>, registry: &Registry) -> Option<Ecosystem> {
    match op {
        Op::Run { task, .. } => Some(registry.by_id(task.source).ecosystem),
        _ => None,
    }
}

/// What the cascade found for a token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dispatch {
    /// One of runner's own verbs.
    Builtin(String),
    /// A command to run.
    Plan(Box<Plan>),
}

/// The binary an installed dependency declares.
pub type DepFn<'a> = dyn Fn(&str) -> Result<Option<PathBuf>, Refusal> + 'a;

/// Asks the user whether a rung may fetch.
pub type ConfirmFn<'a> = dyn Fn(&str, &str) -> bool + 'a;

/// Everything the cascade consults besides the token.
pub struct Cascade<'a> {
    /// The tree the token was typed in.
    pub tree: &'a Tree,
    /// The resolved project.
    pub project: &'a Project,
    /// The override chain.
    pub policy: &'a Policy,
    /// The provider table.
    pub registry: &'a Registry,
    /// runner's own verbs, which the first rung takes.
    pub builtins: &'a [&'static str],
    /// The binary an installed dependency declares, when one does.
    pub dep: Option<&'a DepFn<'a>>,
    /// Asks the user about a rung that can fetch. `None` means nothing can
    /// be asked, so the fetch proceeds.
    pub confirm: Option<&'a ConfirmFn<'a>>,
}

/// Walk the cascade for `token` and return the first plan a rung yields.
///
/// # Errors
///
/// `NotFound` when no rung took the token, `Declined` when a fetching rung
/// was refused, `Ambiguous` when several members define the name, plus the
/// refusals [`plan_with`] makes.
pub fn dispatch(
    cascade: &Cascade<'_>,
    token: &str,
    args: &[String],
) -> Result<(Rung, Dispatch), Refusal> {
    dispatch_from(cascade, CASCADE[0].name, token, args)
}

/// Walk the cascade from the rung named `first`.
///
/// Every earlier rung still counts as tried, since the caller is the one
/// that tried it.
///
/// # Errors
///
/// The refusals [`dispatch`] makes.
pub fn dispatch_from(
    cascade: &Cascade<'_>,
    first: &str,
    token: &str,
    args: &[String],
) -> Result<(Rung, Dispatch), Refusal> {
    let mut tried = Vec::new();
    let mut reached = false;
    for rung in CASCADE {
        tried.push(*rung);
        reached = reached || rung.name == first;
        if !reached {
            continue;
        }
        let Some(found) = rung_dispatch(cascade, *rung, token, args)? else {
            continue;
        };
        if let Dispatch::Plan(made) = &found {
            gate(cascade, *rung, token, made)?;
        }
        return Ok((*rung, found));
    }
    Err(Refusal::NotFound {
        name: token.to_owned(),
        tried,
    })
}

/// What one rung makes of `token`.
fn rung_dispatch(
    cascade: &Cascade<'_>,
    rung: Rung,
    token: &str,
    args: &[String],
) -> Result<Option<Dispatch>, Refusal> {
    let found = |path: PathBuf| {
        plan_found(cascade.tree, cascade.project, cascade.policy, path, args).map(dispatched)
    };

    Ok(match rung.needs {
        Need::BareVerb => builtin(cascade, token).map(Dispatch::Builtin),
        Need::ExplicitPath => {
            if !has_local_prefix(token) {
                return Ok(None);
            }
            let path = resolve_path(&cascade.tree.cwd, token);
            if path.is_dir() {
                return Ok(None);
            }
            Some(dispatched(file_plan(cascade, &path, args)?))
        }
        Need::Task => match select(cascade, token)? {
            Some(task) => Some(dispatched(plan(
                cascade.tree,
                cascade.project,
                cascade.policy,
                &Op::Run { task, args },
                cascade.registry,
            )?)),
            None => None,
        },
        Need::RelativeFile => {
            let path = resolve_path(&cascade.tree.cwd, token);
            if !path.is_file() {
                return Ok(None);
            }
            Some(dispatched(file_plan(cascade, &path, args)?))
        }
        Need::InstalledDep => match cascade.dep.map(|ask| ask(token)).transpose()?.flatten() {
            Some(path) => Some(dispatched(file_plan(cascade, &path, args)?)),
            None => None,
        },
        Need::Cap(Cap::Test) => {
            if token != "test" || select(cascade, token)?.is_some() {
                return Ok(None);
            }
            plan(
                cascade.tree,
                cascade.project,
                cascade.policy,
                &Op::Test { args },
                cascade.registry,
            )
            .map(dispatched)
            .map(Some)
            .or_else(|refusal| match refusal {
                Refusal::NoCapability { .. } => Ok(None),
                _ => Err(refusal),
            })?
        }
        Need::ProjectBins => probe_in_dirs(&bin_dirs(cascade.project, &Scope::Root), token)
            .map(found)
            .transpose()?,
        Need::HostPath => {
            let root = cascade.project.present.iter().find(|p| {
                let provider = cascade.registry.by_id(p.provider);
                provider.program == Some(token) && provider.caps.run_default.is_some()
            });
            if let Some(present) = root {
                Some(dispatched(plan_with(
                    cascade.tree,
                    cascade.project,
                    cascade.policy,
                    present,
                    &Op::RunDefault { args },
                    cascade.registry,
                )?))
            } else {
                probe_with(token, &[]).map(found).transpose()?
            }
        }
        Need::ToolManagerExec | Need::Cap(Cap::Exec) => {
            exec_plan(cascade, rung, token, args)?.map(dispatched)
        }
    })
}

/// A plan as the cascade returns it.
fn dispatched(made: Plan) -> Dispatch {
    Dispatch::Plan(Box::new(made))
}

/// The builtin verb `token` names. Explicit qualifiers bypass this rung.
fn builtin(cascade: &Cascade<'_>, token: &str) -> Option<String> {
    cascade.builtins.contains(&token).then(|| token.to_owned())
}

/// Refuse a fetching plan the policy or the user does not want.
fn gate(cascade: &Cascade<'_>, rung: Rung, name: &str, made: &Plan) -> Result<(), Refusal> {
    if crate::reach::permitted(made.reach, cascade.policy.reach, || {
        cascade.confirm.is_none_or(|ask| ask(name, rung.name))
    }) {
        return Ok(());
    }
    Err(Refusal::Declined {
        name: name.to_owned(),
        rung,
    })
}

/// The task `name` addresses, narrowed to the nearest scope.
///
/// # Errors
///
/// `Ambiguous` when several workspace members define the name and no nearer
/// scope does.
pub fn select<'a>(cascade: &'a Cascade<'_>, token: &str) -> Result<Option<&'a Task>, Refusal> {
    let (mut scope, mut source, mut name) = task_address(cascade, token);
    if !cascade.project.tasks.iter().any(|t| {
        t.name == name
            && source.is_none_or(|s| s == t.source)
            && scope.is_none_or(|s| scope_matches(&t.scope, s))
    }) && cascade.project.tasks.iter().any(|t| t.name == token)
    {
        scope = None;
        source = None;
        name = token;
    }

    let mut found: Vec<&Task> = cascade
        .project
        .tasks
        .iter()
        .filter(|task| {
            task.name == name
                && source.is_none_or(|source| task.source == source)
                && scope.is_none_or(|scope| scope_matches(&task.scope, scope))
        })
        .collect();
    let Some(nearest) = found
        .iter()
        .map(|task| scope_rank(cascade.tree, &task.scope))
        .min()
    else {
        return if source.is_some() || scope.is_some() {
            Err(Refusal::NotFound {
                name: token.into(),
                tried: CASCADE[..3].to_vec(),
            })
        } else {
            Ok(None)
        };
    };
    found.retain(|task| scope_rank(cascade.tree, &task.scope) == nearest);
    let mut members: Vec<&Scope> = Vec::new();
    for scope in found.iter().map(|task| &task.scope) {
        if matches!(scope, Scope::Member { .. }) && !members.contains(&scope) {
            members.push(scope);
        }
    }
    if members.len() > 1 {
        return Err(Refusal::Ambiguous {
            candidates: found
                .iter()
                .map(|task| (task.source, task.scope.clone()))
                .collect(),
        });
    }
    found.sort_by_key(|task| task_rank(cascade.policy, cascade.registry, task));
    Ok(found.first().copied())
}

fn scope_matches(scope: &Scope, spelling: &str) -> bool {
    match scope {
        Scope::Root => spelling == "root",
        Scope::Member { name, dir } => name == spelling || dir.ends_with(spelling),
    }
}

fn task_address<'a>(
    cascade: &Cascade<'_>,
    token: &'a str,
) -> (Option<&'a str>, Option<ProviderId>, &'a str) {
    let source = |label| {
        cascade
            .registry
            .by_label(label)
            .filter(|p| p.kind.contains(Kind::TASK_SOURCE))
            .map(|p| p.id)
    };
    if let Some((prefix, name)) = token.split_once('#') {
        let (scope, label) = prefix
            .split_once(':')
            .map_or((None, prefix), |(s, l)| (Some(s), l));
        if let Some(id) = source(label) {
            return (scope, Some(id), name);
        }
    }
    if let Some((prefix, rest)) = token.split_once(':') {
        if let Some(id) = source(prefix) {
            return (None, Some(id), rest);
        }
        if prefix == "root"
            || cascade
                .tree
                .members
                .iter()
                .any(|s| scope_matches(s, prefix))
        {
            if let Some((label, name)) = rest.split_once(':')
                && let Some(id) = source(label)
            {
                return (Some(prefix), Some(id), name);
            }
            return (Some(prefix), None, rest);
        }
    }
    (None, None, token)
}

/// How far a scope is from the invocation directory: the member holding it,
/// then the root, then every other member.
fn scope_rank(tree: &Tree, scope: &Scope) -> u8 {
    match scope {
        Scope::Member { dir, .. } if tree.cwd.starts_with(dir) => 0,
        Scope::Root => 1,
        Scope::Member { .. } => 2,
    }
}

/// The order same-named tasks are tried: the runner policy chose, then the
/// sources its package manager or runtime dispatches, then registry order,
/// with aliases last.
fn task_rank(
    policy: &Policy,
    registry: &Registry,
    task: &Task,
) -> (usize, usize, u8, ProviderId, bool) {
    let pinned = if policy.runner.is_none()
        && policy
            .pm
            .0
            .values()
            .all(|choice| choice.from == Layer::Probe)
    {
        policy
            .task_sources
            .get(&task.name)
            .and_then(|sources| sources.iter().position(|id| *id == task.source))
            .unwrap_or(usize::MAX)
    } else {
        usize::MAX
    };
    let chosen = policy
        .runner
        .as_ref()
        .is_some_and(|choice| choice.id == task.source);
    let dispatched = policy
        .pm
        .0
        .values()
        .chain(policy.runtime.iter())
        .filter(|choice| choice.from != Layer::Probe)
        .any(|choice| {
            registry
                .by_id(choice.id)
                .caps
                .run_task
                .is_some_and(|cap| cap.sources.contains(&task.source))
        });
    let tier = if chosen {
        0
    } else if let Some(rank) = policy.prefer.iter().position(|id| *id == task.source) {
        rank + 1
    } else {
        policy.prefer.len() + 1 + usize::from(!dispatched)
    };
    (
        pinned,
        tier,
        registry.by_id(task.source).caps.task_priority,
        task.source,
        task.alias_of.is_some(),
    )
}

/// The exec plan for `rung`: a tool manager's primitive, or any other present
/// provider's.
fn exec_plan(
    cascade: &Cascade<'_>,
    rung: Rung,
    name: &str,
    args: &[String],
) -> Result<Option<Plan>, Refusal> {
    let manager = rung.needs == Need::ToolManagerExec;
    let op = Op::Exec { name, args };
    for present in &cascade.project.present {
        let provider = cascade
            .registry
            .by_id(present.provider)
            .for_present(present);
        if provider.kind == Kind::RUNTIME
            && cascade
                .policy
                .runtime
                .as_ref()
                .is_none_or(|choice| choice.id != provider.id)
        {
            continue;
        }
        if provider.kind.contains(Kind::TOOL_MANAGER) != manager {
            continue;
        }
        if provider.caps.exec.is_none_or(|cap| cap.reach != rung.reach) {
            continue;
        }
        match plan_with(
            cascade.tree,
            cascade.project,
            cascade.policy,
            present,
            &op,
            cascade.registry,
        ) {
            Ok(made) => return Ok(Some(made)),
            Err(Refusal::Unsafe(Unsafe::NameShape { .. }) | Refusal::NoCapability { .. }) => {}
            Err(refusal) => return Err(refusal),
        }
    }
    Ok(None)
}

/// The path `token` names, relative to `base`, with a leading `~` expanded
/// and `.` components dropped.
#[must_use]
pub fn resolve_path(base: &Path, token: &str) -> PathBuf {
    let expanded = token.strip_prefix('~').map_or_else(
        || PathBuf::from(token),
        |rest| {
            home().map_or_else(
                || PathBuf::from(token),
                |home| home.join(rest.trim_start_matches(['/', '\\'])),
            )
        },
    );
    let joined = if expanded.is_absolute() {
        expanded
    } else {
        base.join(expanded)
    };
    joined
        .components()
        .filter(|part| !matches!(part, std::path::Component::CurDir))
        .collect()
}

/// The user's home directory.
fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// Whether `token` names a path the user spelled out.
#[must_use]
pub fn has_local_prefix(token: &str) -> bool {
    token.starts_with("./")
        || token.starts_with("../")
        || token.starts_with(".\\")
        || token.starts_with("..\\")
        || token.starts_with('/')
        || token.starts_with('\\')
        || token.starts_with('~')
        || windows_drive_abs(token)
}

/// Whether `token` starts with a Windows drive root.
const fn windows_drive_abs(token: &str) -> bool {
    let bytes = token.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'/' || bytes[2] == b'\\')
}

/// The first executable named `name` in `dirs` alone.
fn probe_in_dirs(dirs: &[PathBuf], name: &str) -> Option<PathBuf> {
    if dirs.is_empty() {
        return None;
    }
    let joined = std::env::join_paths(dirs.iter().cloned()).ok()?;
    probe_in(name, &joined, std::env::var_os("PATHEXT").as_deref())
}

/// The interpreter and arguments in a file's shebang.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shebang {
    /// Interpreter program.
    pub program: String,
    /// Arguments, respecting the kernel's single argument and `env -S` splitting.
    pub args: Vec<String>,
}

/// Read only the bounded shebang header of a source file.
#[must_use]
pub fn read_shebang(path: &Path) -> Option<Shebang> {
    use std::io::Read as _;
    let mut head = [0; 512];
    let read = std::fs::File::open(path).ok()?.read(&mut head).ok()?;
    let text = std::str::from_utf8(&head[..read]).ok()?;
    parse_shebang(text.lines().next()?)
}

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
        // `env -S` / `--split-string` re-splits its single kernel argument with
        // env's own quote-aware parser, so quoted args (`-S deno run
        // --allow-read="a b"`) stay one token. Plain `env` (no `-S`) does NOT
        // split: the kernel's single argument is the program name, so
        // `#!/usr/bin/env python3 -O` resolves to the (bogus) program
        // `"python3 -O"` exactly as the kernel would run it; `-S` is what adds
        // the splitting.
        let split_form = rest
            .strip_prefix("--split-string=")
            .or_else(|| rest.strip_prefix("--split-string"))
            .or_else(|| rest.strip_prefix("-S"))
            .map(str::trim);
        let words = split_form.map_or_else(|| single_arg(rest), split_env_string);
        let mut parts = words.into_iter();
        let program = parts.next()?;
        let args = parts.collect();
        return Some(Shebang { program, args });
    }

    // Direct interpreter: the kernel passes the whole remainder as one argument.
    Some(Shebang {
        program: interpreter.to_string(),
        args: single_arg(rest),
    })
}

/// The interpreter's single argument as the kernel passes it: the entire
/// remainder kept intact (internal spaces and all), or no argument at all when
/// the remainder is empty. Used by plain `env` and direct-interpreter shebangs,
/// neither of which the kernel whitespace-splits.
fn single_arg(rest: &str) -> Vec<String> {
    if rest.is_empty() {
        Vec::new()
    } else {
        vec![rest.to_owned()]
    }
}

/// Split an `env -S` / `--split-string` argument the way GNU `env`'s
/// split-string parser does for the cases shebangs actually use: ASCII
/// whitespace separates words, while single quotes, double quotes, and a
/// backslash keep a run, embedded spaces and all, as one word. This
/// preserves quoted arguments (`--allow-read="a b"`) that `split_whitespace`
/// would tear in two. The exotic `\t`/`\n`/`\c` escape sequences env also
/// understands are vanishingly rare in shebangs and intentionally unmodeled.
fn split_env_string(s: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut started = false;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            c if c.is_ascii_whitespace() => {
                if started {
                    words.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            '\'' => {
                started = true;
                for quoted in chars.by_ref() {
                    if quoted == '\'' {
                        break;
                    }
                    current.push(quoted);
                }
            }
            '"' => {
                started = true;
                while let Some(quoted) = chars.next() {
                    match quoted {
                        '"' => break,
                        '\\' => match chars.next() {
                            Some(esc @ ('"' | '\\' | '$' | '`')) => current.push(esc),
                            Some(other) => {
                                current.push('\\');
                                current.push(other);
                            }
                            None => current.push('\\'),
                        },
                        _ => current.push(quoted),
                    }
                }
            }
            '\\' => {
                started = true;
                if let Some(escaped) = chars.next() {
                    current.push(escaped);
                }
            }
            _ => {
                started = true;
                current.push(c);
            }
        }
    }
    if started {
        words.push(current);
    }
    words
}

/// Whether `interpreter` is the `env` launcher (by file name).
fn is_env(interpreter: &str) -> bool {
    Path::new(interpreter)
        .file_name()
        .is_some_and(|name| name == "env")
}

/// Whether the kernel can spawn `path` by itself.
#[must_use]
pub fn is_directly_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        std::fs::metadata(path).is_ok_and(|meta| meta.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        path.extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| {
                ["exe", "com", "bat", "cmd"]
                    .iter()
                    .any(|known| ext.eq_ignore_ascii_case(known))
            })
    }
}

/// The plan for a file on disk: the kernel's, its `#!` line's, or the runtime
/// that takes its extension.
///
/// # Errors
///
/// `NotFound` when the path is not a file, `NoCapability` when no present
/// runtime runs its extension.
pub fn file_plan(cascade: &Cascade<'_>, path: &Path, args: &[String]) -> Result<Plan, Refusal> {
    let mut made = file_plan_inner(cascade, path, args)?;
    made.found = Some(path.to_path_buf());
    if !made.because.iter().any(|e| e.at == path) {
        made.because.push(Evidence {
            provider: None,
            signal: None,
            at: path.to_path_buf(),
            scope: made.scope.clone(),
            weight: Weight::Present,
            declared: None,
        });
    }
    Ok(made)
}

fn file_plan_inner(cascade: &Cascade<'_>, path: &Path, args: &[String]) -> Result<Plan, Refusal> {
    if !path.is_file() {
        return Err(Refusal::NotFound {
            name: path.display().to_string(),
            tried: Vec::new(),
        });
    }
    let shebang = read_shebang(path);
    if let Some(choice) = &cascade.policy.runtime
        && let Some(present) = cascade
            .project
            .present
            .iter()
            .find(|p| p.provider == choice.id)
    {
        let provider = cascade.registry.by_id(choice.id);
        let extension_matches = path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| {
                provider
                    .caps
                    .run_file
                    .is_some_and(|cap| cap.extensions.contains(&ext))
            });
        let interpreter_matches = shebang.as_ref().is_some_and(|s| {
            Path::new(&s.program)
                .file_name()
                .and_then(|p| p.to_str())
                .is_some_and(|p| provider.caps.file_interpreters.contains(&p))
        });
        if extension_matches || interpreter_matches {
            return plan_with(
                cascade.tree,
                cascade.project,
                cascade.policy,
                present,
                &Op::RunFile { file: path, args },
                cascade.registry,
            );
        }
    }
    let routed = path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| runs_extension(cascade, ext));
    if is_directly_executable(path) && (shebang.is_some() || !routed) {
        return plan_found(
            cascade.tree,
            cascade.project,
            cascade.policy,
            path.to_path_buf(),
            args,
        );
    }
    if let Some(shebang) = shebang {
        let mut words = vec![OsString::from(&shebang.program)];
        words.extend(shebang.args.into_iter().map(OsString::from));
        words.push(OsString::from(path));
        words.extend(args.iter().map(OsString::from));
        return plan_argv(
            cascade.tree,
            cascade.project,
            cascade.policy,
            path.to_path_buf(),
            words,
        );
    }
    runtime_file_plan(cascade, path, args)
}

fn runtime_file_plan(cascade: &Cascade<'_>, path: &Path, args: &[String]) -> Result<Plan, Refusal> {
    let op = Op::RunFile { file: path, args };
    match plan(
        cascade.tree,
        cascade.project,
        cascade.policy,
        &op,
        cascade.registry,
    ) {
        Ok(made) => Ok(made),
        Err(refusal @ Refusal::NoCapability { .. }) => {
            for provider in cascade.registry.iter().filter(|p| p.caps.file_fallback) {
                let scope = cascade
                    .tree
                    .members
                    .iter()
                    .filter(
                        |scope| matches!(scope, Scope::Member { dir, .. } if path.starts_with(dir)),
                    )
                    .max_by_key(|scope| scope_dir(cascade.tree, scope).components().count())
                    .cloned()
                    .unwrap_or(Scope::Root);
                let present = Present {
                    provider: provider.id,
                    scope: scope.clone(),
                    version: None,
                    bin_dirs: vec![],
                    because: vec![Evidence {
                        provider: None,
                        signal: None,
                        at: path.to_path_buf(),
                        scope,
                        weight: Weight::Present,
                        declared: None,
                    }],
                };
                match plan_with(
                    cascade.tree,
                    cascade.project,
                    cascade.policy,
                    &present,
                    &op,
                    cascade.registry,
                ) {
                    Ok(mut made) => {
                        made.found = Some(path.to_path_buf());
                        return Ok(made);
                    }
                    Err(Refusal::NoCapability { .. }) => {}
                    Err(error) => return Err(error),
                }
            }
            Err(refusal)
        }
        Err(error) => Err(error),
    }
}

/// Whether a present provider runs files with this extension.
fn runs_extension(cascade: &Cascade<'_>, ext: &str) -> bool {
    cascade.registry.iter().any(|provider| {
        (provider.caps.file_fallback
            || cascade
                .project
                .present
                .iter()
                .any(|present| present.provider == provider.id))
            && provider
                .caps
                .run_file
                .is_some_and(|cap| cap.extensions.contains(&ext))
    })
}
#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::{
        Cascade, Dispatch, NameShape, Plan, Refusal, Shebang, Trust, Unsafe, discover, dispatch,
        plan, read_shebang,
    };
    use crate::capability::{
        BinDirs, BinsCap, Capabilities, Discovery, ExecCap, QuietSupport, RunFileCap, RunTaskCap,
        RuntimeCap, TestCap,
    };
    use crate::cascade::{CASCADE, Rung};
    use crate::evidence::{Evidence, Present, Weight};
    use crate::op::Op;
    use crate::policy::{Choice, Layer, Policy, ReachPolicy};
    use crate::probe::tests::TempDir;
    use crate::provider::{Ecosystem, Hooks, Kind, ProviderId};
    use crate::reach::Reach;
    use crate::registry::{Provider, Registry};
    use crate::resolve::Project;
    use crate::scope::Scope;
    use crate::signal::{Signal, SignalId};
    use crate::t;
    use crate::task::{Task, TaskDetail};
    use crate::tree::Tree;
    use crate::verbosity::Verbosity;

    static FAKES: &[Provider] = &[
        Provider {
            id: ProviderId::Npm,
            label: "npm",
            aliases: &[],
            ecosystem: Ecosystem::Node,
            kind: Kind::PACKAGE_MANAGER,
            program: Some("npm"),
            signals: &[Signal::Probe("npm")],
            writes: &[],
            caps: Capabilities {
                run_task: Some(RunTaskCap {
                    argv: t![Quiet, "run", Task, Sep("--"), Args],
                    sources: &[ProviderId::PackageJson],
                }),
                exec: Some(ExecCap {
                    program: Some("npx"),
                    argv: t![Name, Args],
                    reach: Reach::Network,
                    accepts: NameShape::BARE.union(NameShape::VERSIONED),
                }),
                test: Some(TestCap {
                    program: Some("node"),
                    argv: t!["--test", Args, Files],
                    discovery: Discovery::Files(&["test.js", "*.test.js"]),
                }),
                bins: Some(BinsCap {
                    dirs: BinDirs::Static(&["node_modules/.bin"]),
                }),
                quiet: QuietSupport::flag(t!["--silent"]),
                ..Capabilities::NONE
            },
            tasks: None,
            version: None,
            hooks: Hooks::NONE,
        },
        Provider {
            id: ProviderId::Bun,
            label: "bun",
            aliases: &[],
            ecosystem: Ecosystem::Node,
            kind: Kind::PACKAGE_MANAGER.union(Kind::RUNTIME),
            program: Some("bun"),
            signals: &[Signal::Probe("bun")],
            writes: &[],
            caps: Capabilities {
                run_task: Some(RunTaskCap {
                    argv: t!["run", Task, Args],
                    sources: &[ProviderId::PackageJson],
                }),
                exec: Some(ExecCap {
                    program: None,
                    argv: t!["x", Name, Args],
                    reach: Reach::Network,
                    accepts: NameShape::BARE,
                }),
                run_file: Some(RunFileCap {
                    program: None,
                    extensions: &["ts", "js"],
                    argv: t![File, Args],
                }),
                as_runtime: Some(RuntimeCap {
                    package_exec: None,
                    run_task: Some(t!["--bun", "run", Task, Args]),
                    exec: Some(t!["x", "--bun", Name, Args]),
                }),
                quiet: QuietSupport::unsupported("none"),
                ..Capabilities::NONE
            },
            tasks: None,
            version: None,
            hooks: Hooks::NONE,
        },
        Provider {
            id: ProviderId::Go,
            label: "go",
            aliases: &[],
            ecosystem: Ecosystem::Go,
            kind: Kind::PACKAGE_MANAGER,
            program: Some("go"),
            signals: &[Signal::Probe("go")],
            writes: &[],
            caps: Capabilities {
                exec: Some(ExecCap {
                    program: None,
                    argv: t!["run", Name, Args],
                    reach: Reach::Network,
                    accepts: NameShape::PATH_LIKE.union(NameShape::VERSIONED),
                }),
                ..Capabilities::NONE
            },
            tasks: None,
            version: None,
            hooks: Hooks::NONE,
        },
        Provider {
            id: ProviderId::PackageJson,
            label: "package.json",
            aliases: &[],
            ecosystem: Ecosystem::Node,
            kind: Kind::TASK_SOURCE,
            program: None,
            signals: &[Signal::File("package.json")],
            writes: &[],
            caps: Capabilities::NONE,
            tasks: None,
            version: None,
            hooks: Hooks::NONE,
        },
    ];

    fn tree() -> Tree {
        Tree {
            cwd: PathBuf::from("/p"),
            root: PathBuf::from("/p"),
            members: Vec::new(),
        }
    }

    fn present(provider: ProviderId, weight: Weight) -> Present {
        Present {
            provider,
            scope: Scope::Root,
            version: None,
            bin_dirs: vec![PathBuf::from("/p/node_modules/.bin")],
            because: vec![Evidence {
                provider: Some(provider),
                signal: Some(SignalId(0)),
                at: PathBuf::from("/p/lock"),
                scope: Scope::Root,
                weight,
                declared: None,
            }],
        }
    }

    fn task(name: &str) -> Task {
        Task {
            name: name.to_owned(),
            source: ProviderId::PackageJson,
            scope: Scope::Root,
            target: None,
            description: None,
            alias_of: None,
            forwards_to: None,
            detail: TaskDetail::default(),
        }
    }

    fn words(plan: &Plan) -> Vec<String> {
        plan.argv
            .iter()
            .map(|word| word.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn name_shapes_follow_the_exec_primitives() {
        assert_eq!(NameShape::of("eslint"), NameShape::BARE);
        assert_eq!(NameShape::of("@scope/pkg"), NameShape::BARE);
        assert_eq!(NameShape::of("eslint@9"), NameShape::VERSIONED);
        assert_eq!(NameShape::of("@scope/pkg@1"), NameShape::VERSIONED);
        assert_eq!(NameShape::of("user/repo#ref"), NameShape::PATH_LIKE);
        assert_eq!(NameShape::of("./cmd/foo"), NameShape::PATH_LIKE);
        assert_eq!(
            NameShape::of("golang.org/x/tools/cmd/godoc@latest"),
            NameShape::PATH_LIKE
        );
    }

    #[test]
    fn a_task_runs_through_the_provider_that_dispatches_its_source() {
        let registry = Registry(FAKES);
        let project = Project {
            present: vec![present(ProviderId::Npm, Weight::Locked)],
            tasks: vec![task("build")],
            warnings: Vec::new(),
        };
        let args = ["--watch".to_owned()];
        let policy = Policy {
            verbosity: Verbosity::Quiet,
            ..Policy::default()
        };
        let made = plan(
            &tree(),
            &project,
            &policy,
            &Op::Run {
                task: &project.tasks[0],
                args: &args,
            },
            &registry,
        )
        .expect("npm runs package.json scripts");
        assert_eq!(
            words(&made),
            ["npm", "--silent", "run", "build", "--", "--watch"]
        );
        assert_eq!(made.provider, Some(ProviderId::Npm));
        assert_eq!(made.trust, Trust::Project);
        assert_eq!(made.path_prepend, [PathBuf::from("/p/node_modules/.bin")]);
        assert_eq!(made.decided_by, [Layer::Lockfile(PathBuf::from("/p/lock"))]);
        assert!(made.clamps.is_empty());
        assert_eq!(made.because.len(), 1);
    }

    #[test]
    fn the_runtime_choice_takes_its_own_form_and_records_a_clamp() {
        let registry = Registry(FAKES);
        let project = Project {
            present: vec![
                present(ProviderId::Npm, Weight::Locked),
                present(ProviderId::Bun, Weight::Probed),
            ],
            tasks: vec![task("build")],
            warnings: Vec::new(),
        };
        let policy = Policy {
            runtime: Some(Choice {
                id: ProviderId::Bun,
                from: Layer::Cli,
            }),
            verbosity: Verbosity::Silent,
            ..Policy::default()
        };
        let run = plan(
            &tree(),
            &project,
            &policy,
            &Op::Run {
                task: &project.tasks[0],
                args: &[],
            },
            &registry,
        )
        .expect("bun as the runtime");
        assert_eq!(words(&run), ["bun", "--bun", "run", "build"]);
        assert_eq!(run.decided_by, [Layer::Cli]);
        assert_eq!(run.clamps.len(), 1);
        assert_eq!(run.clamps[0].requested, "silent");
        assert_eq!(run.clamps[0].granted, "normal");
        let exec = plan(
            &tree(),
            &project,
            &policy,
            &Op::Exec {
                name: "eslint",
                args: &[],
            },
            &registry,
        )
        .expect("bun as the exec runtime");
        assert_eq!(words(&exec), ["bun", "x", "--bun", "eslint"]);
        assert_eq!(exec.reach, Reach::Network);
    }

    #[test]
    fn a_name_of_the_wrong_shape_never_reaches_the_primitive() {
        let registry = Registry(FAKES);
        let project = Project {
            present: vec![present(ProviderId::Npm, Weight::Locked)],
            tasks: Vec::new(),
            warnings: Vec::new(),
        };
        let refusal = plan(
            &tree(),
            &project,
            &Policy::default(),
            &Op::Exec {
                name: "user/repo#ref",
                args: &[],
            },
            &registry,
        )
        .expect_err("npx never sees a path");
        assert_eq!(
            refusal,
            Refusal::Unsafe(Unsafe::NameShape {
                name: "user/repo#ref".to_owned(),
                provider: ProviderId::Npm,
            })
        );
        let go = Project {
            present: vec![present(ProviderId::Go, Weight::Locked)],
            tasks: Vec::new(),
            warnings: Vec::new(),
        };
        let made = plan(
            &tree(),
            &go,
            &Policy::default(),
            &Op::Exec {
                name: "golang.org/x/tools/cmd/godoc@latest",
                args: &[],
            },
            &registry,
        )
        .expect("go run takes a path");
        assert_eq!(
            words(&made),
            ["go", "run", "golang.org/x/tools/cmd/godoc@latest"]
        );
    }

    #[test]
    fn a_test_runner_is_handed_the_files_it_asked_for() {
        let dir = TempDir::new("discover");
        fs::create_dir_all(dir.path().join("src/node_modules/x")).expect("dirs");
        fs::write(dir.path().join("src/a.test.js"), "").expect("file");
        fs::write(dir.path().join("test.js"), "").expect("file");
        fs::write(dir.path().join("src/node_modules/x/b.test.js"), "").expect("file");
        fs::write(dir.path().join("src/atest.js"), "").expect("file");
        let found = discover(dir.path(), &["test.js", "*.test.js"]);
        assert_eq!(
            found,
            [PathBuf::from("src/a.test.js"), PathBuf::from("test.js")]
        );
        let registry = Registry(FAKES);
        let project = Project {
            present: vec![present(ProviderId::Npm, Weight::Locked)],
            tasks: Vec::new(),
            warnings: Vec::new(),
        };
        let tree = Tree {
            cwd: dir.path().to_path_buf(),
            root: dir.path().to_path_buf(),
            members: Vec::new(),
        };
        let made = plan(
            &tree,
            &project,
            &Policy::default(),
            &Op::Test { args: &[] },
            &registry,
        )
        .expect("node --test over the files");
        assert_eq!(words(&made), ["node", "--test", "src/a.test.js", "test.js"]);
        let empty = TempDir::new("discover-empty");
        let tree = Tree {
            cwd: empty.path().to_path_buf(),
            root: empty.path().to_path_buf(),
            members: Vec::new(),
        };
        assert_eq!(
            plan(
                &tree,
                &project,
                &Policy::default(),
                &Op::Test { args: &[] },
                &registry
            ),
            Err(Refusal::NoCapability {
                provider: ProviderId::Npm,
                op: "test"
            })
        );
    }

    #[test]
    fn a_project_trust_env_layer_cannot_set_a_loader_hook() {
        let registry = Registry(FAKES);
        let project = Project {
            present: vec![present(ProviderId::Npm, Weight::Locked)],
            tasks: vec![task("build")],
            warnings: Vec::new(),
        };
        let mut policy = Policy::default();
        policy
            .env
            .project
            .insert("NODE_OPTIONS".to_owned(), "--require x".to_owned());
        let refusal = plan(
            &tree(),
            &project,
            &policy,
            &Op::Run {
                task: &project.tasks[0],
                args: &[],
            },
            &registry,
        )
        .expect_err("loader hooks are refused");
        assert_eq!(
            refusal,
            Refusal::Unsafe(Unsafe::LoaderHook {
                name: "NODE_OPTIONS".to_owned()
            })
        );
        policy.trust = crate::policy::TrustPolicy::Full;
        let made = plan(
            &tree(),
            &project,
            &policy,
            &Op::Run {
                task: &project.tasks[0],
                args: &[],
            },
            &registry,
        )
        .expect("user trust may set anything");
        assert_eq!(made.env.len(), 1);
    }

    fn cascade<'a>(
        tree: &'a Tree,
        project: &'a Project,
        policy: &'a Policy,
        registry: &'a Registry,
    ) -> Cascade<'a> {
        Cascade {
            tree,
            project,
            policy,
            registry,
            builtins: &["list", "install"],
            dep: None,
            confirm: None,
        }
    }

    fn rung_of(found: &(Rung, Dispatch)) -> &'static str {
        found.0.name
    }

    fn argv(found: &(Rung, Dispatch)) -> Vec<String> {
        match &found.1 {
            Dispatch::Plan(made) => words(made),
            Dispatch::Builtin(name) => vec![name.clone()],
        }
    }

    #[test]
    fn a_builtin_verb_always_precedes_a_same_named_task() {
        let registry = Registry(FAKES);
        let policy = Policy::default();
        let tree = tree();
        let bare = Project {
            present: vec![present(ProviderId::Npm, Weight::Locked)],
            tasks: Vec::new(),
            warnings: Vec::new(),
        };
        let found = dispatch(&cascade(&tree, &bare, &policy, &registry), "list", &[])
            .expect("the builtin rung takes it");
        assert_eq!(rung_of(&found), "builtin");
        assert_eq!(found.1, Dispatch::Builtin("list".to_owned()));

        let shadowed = Project {
            present: vec![present(ProviderId::Npm, Weight::Locked)],
            tasks: vec![task("list")],
            warnings: Vec::new(),
        };
        let found = dispatch(&cascade(&tree, &shadowed, &policy, &registry), "list", &[])
            .expect("the builtin rung takes it despite the task");
        assert_eq!(rung_of(&found), "builtin");
        assert_eq!(found.1, Dispatch::Builtin("list".to_owned()));
    }

    #[test]
    fn a_miss_lists_every_rung_it_tried_in_order() {
        let registry = Registry(FAKES);
        let policy = Policy::default();
        let tree = Tree {
            cwd: std::env::temp_dir().join("runner-core-no-such-dir"),
            root: std::env::temp_dir().join("runner-core-no-such-dir"),
            members: Vec::new(),
        };
        let project = Project::default();
        let refusal = dispatch(
            &cascade(&tree, &project, &policy, &registry),
            "runner-core-no-such-token",
            &[],
        )
        .expect_err("nothing takes it");
        let Refusal::NotFound { name, tried } = refusal else {
            panic!("a miss is not found");
        };
        assert_eq!(name, "runner-core-no-such-token");
        assert_eq!(
            tried.iter().map(|rung| rung.name).collect::<Vec<_>>(),
            CASCADE.iter().map(|rung| rung.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn the_reach_gate_refuses_a_fetching_rung_and_asks_before_one() {
        let registry = Registry(FAKES);
        let tree = tree();
        let project = Project {
            present: vec![present(ProviderId::Npm, Weight::Locked)],
            tasks: Vec::new(),
            warnings: Vec::new(),
        };
        let local = Policy {
            reach: ReachPolicy::Local,
            ..Policy::default()
        };
        let refusal = dispatch(
            &cascade(&tree, &project, &local, &registry),
            "runner-core-no-such-tool",
            &[],
        )
        .expect_err("a local policy refuses the exec rung");
        assert!(matches!(
            refusal,
            Refusal::Declined { ref rung, .. } if rung.name == "exec"
        ));

        let allow = Policy {
            reach: ReachPolicy::Allow,
            ..Policy::default()
        };
        let found = dispatch(
            &cascade(&tree, &project, &allow, &registry),
            "runner-core-no-such-tool",
            &[],
        )
        .expect("allow proceeds");
        assert_eq!(rung_of(&found), "exec");
        assert_eq!(argv(&found), ["npx", "runner-core-no-such-tool"]);

        let asked = std::cell::Cell::new(0);
        let refuse = |_: &str, _: &str| {
            asked.set(asked.get() + 1);
            false
        };
        let ask = Policy::default();
        let mut with_prompt = cascade(&tree, &project, &ask, &registry);
        with_prompt.confirm = Some(&refuse);
        let refusal = dispatch(&with_prompt, "runner-core-no-such-tool", &[])
            .expect_err("a declined prompt refuses");
        assert!(matches!(refusal, Refusal::Declined { .. }));
        assert_eq!(asked.get(), 1);
    }

    #[test]
    fn a_root_task_is_visible_from_a_member_and_the_member_wins_its_own_name() {
        let registry = Registry(FAKES);
        let policy = Policy::default();
        let member = PathBuf::from("/p/apps/web");
        let tree = Tree {
            cwd: member.clone(),
            root: PathBuf::from("/p"),
            members: vec![Scope::Member {
                name: "web".to_owned(),
                dir: member.clone(),
            }],
        };
        let scoped = |name: &str, scope: Scope| Task {
            scope,
            ..task(name)
        };
        let project = Project {
            present: vec![present(ProviderId::Npm, Weight::Locked)],
            tasks: vec![
                scoped("build", Scope::Root),
                scoped(
                    "build",
                    Scope::Member {
                        name: "web".to_owned(),
                        dir: member.clone(),
                    },
                ),
                scoped("release", Scope::Root),
            ],
            warnings: Vec::new(),
        };
        let cascade = cascade(&tree, &project, &policy, &registry);
        let found = dispatch(&cascade, "build", &[]).expect("the member's own task wins");
        let Dispatch::Plan(made) = &found.1 else {
            panic!("a task is a plan");
        };
        assert_eq!(made.scope.label(), "web");
        assert_eq!(made.cwd, member);

        let found = dispatch(&cascade, "release", &[]).expect("a root task is visible");
        let Dispatch::Plan(made) = &found.1 else {
            panic!("a task is a plan");
        };
        assert_eq!(made.scope, Scope::Root);
        assert_eq!(made.cwd, PathBuf::from("/p"));
    }

    #[test]
    fn a_name_two_members_define_is_ambiguous() {
        let registry = Registry(FAKES);
        let policy = Policy::default();
        let tree = tree();
        let member = |name: &str| Scope::Member {
            name: name.to_owned(),
            dir: PathBuf::from("/p").join(name),
        };
        let project = Project {
            present: vec![present(ProviderId::Npm, Weight::Locked)],
            tasks: vec![
                Task {
                    scope: member("web"),
                    ..task("build")
                },
                Task {
                    scope: member("api"),
                    ..task("build")
                },
            ],
            warnings: Vec::new(),
        };
        let refusal = dispatch(&cascade(&tree, &project, &policy, &registry), "build", &[])
            .expect_err("two members, no winner");
        let Refusal::Ambiguous { candidates } = refusal else {
            panic!("a tie is ambiguous");
        };
        assert_eq!(candidates.len(), 2);
    }

    #[test]
    fn a_local_file_runs_on_the_runtime_that_takes_its_extension() {
        let dir = TempDir::new("file-plan");
        fs::write(dir.path().join("main.ts"), "console.log(1)\n").expect("source");
        let script = dir.path().join("tool.sh");
        fs::write(&script, "#!/usr/bin/env -S bash -e\necho hi\n").expect("script");
        let registry = Registry(FAKES);
        let policy = Policy::default();
        let tree = Tree {
            cwd: dir.path().to_path_buf(),
            root: dir.path().to_path_buf(),
            members: Vec::new(),
        };
        let project = Project {
            present: vec![present(ProviderId::Bun, Weight::Locked)],
            tasks: Vec::new(),
            warnings: Vec::new(),
        };
        let cascade = Cascade {
            builtins: &[],
            ..cascade(&tree, &project, &policy, &registry)
        };

        let found = dispatch(&cascade, "./main.ts", &[]).expect("the path rung takes it");
        assert_eq!(rung_of(&found), "path");
        assert_eq!(
            argv(&found),
            ["bun", dir.path().join("main.ts").to_string_lossy().as_ref()]
        );

        let found = dispatch(&cascade, "main.ts", &[]).expect("the file rung takes it");
        assert_eq!(rung_of(&found), "file");

        let found = dispatch(&cascade, "tool.sh", &[]).expect("the shebang names the interpreter");
        assert_eq!(
            argv(&found),
            ["bash", "-e", script.to_string_lossy().as_ref()]
        );
    }

    #[test]
    fn a_shebang_parses_every_env_spelling() {
        let dir = TempDir::new("shebang");
        let cases: [(&str, &str, Option<&str>); 5] = [
            ("#!/bin/sh\n", "/bin/sh", None),
            ("#!/usr/bin/python3 -u\n", "/usr/bin/python3", Some("-u")),
            ("#!/usr/bin/env node\n", "node", None),
            (
                "#!/usr/bin/env -S deno run --quiet\n",
                "deno",
                Some("run --quiet"),
            ),
            ("#!/usr/bin/env -Sbun run\n", "bun", Some("run")),
        ];
        for (index, (line, program, arg)) in cases.into_iter().enumerate() {
            let path = dir.path().join(format!("s{index}"));
            fs::write(&path, line).expect("script");
            assert_eq!(
                read_shebang(&path),
                Some(Shebang {
                    program: program.to_owned(),
                    args: arg
                        .into_iter()
                        .flat_map(str::split_whitespace)
                        .map(ToOwned::to_owned)
                        .collect(),
                }),
                "{line}"
            );
        }
        let plain = dir.path().join("plain");
        fs::write(&plain, "echo hi\n").expect("script");
        assert_eq!(read_shebang(&plain), None);
    }
}
