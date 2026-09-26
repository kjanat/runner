//! Shared types used across detection, commands, and tool modules.

use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use runner_core::{Ecosystem, ProviderId};

use crate::provider::Named;

/// A runnable task extracted from a project config file.
#[derive(Debug, Clone)]
pub(crate) struct Task {
    /// Name as it appears in the config (e.g. `"dev"`, `"build"`).
    pub name: String,
    /// Which config file this task was extracted from.
    pub source: ProviderId,
    /// Tool-specific execution target. Used by Go packages to keep the
    /// display name separate from the `go run` target (`.` vs `./cmd/name`).
    pub run_target: Option<String>,
    /// Optional human-readable description (e.g. justfile doc comment,
    /// go-task `desc` field).
    pub description: Option<String>,
    /// When this task is an alias, the name of the target recipe it
    /// resolves to (e.g. `alias b := build` → `Some("build")`).
    pub alias_of: Option<String>,
    /// `Some(runner)` when this task's command body is a thin
    /// passthrough to a task runner for a same-named target, e.g. a
    /// `package.json` script `"build": "just build"` records
    /// `Some(ProviderId::Just)`. Set during detection by inspecting the
    /// actual script body, not inferred from name collisions, so real
    /// scripts like `"build": "vite build"` are never flagged. Used by
    /// completion to avoid emitting a redundant `package.json:build`
    /// candidate alongside the underlying runner's `build` task.
    pub passthrough_to: Option<ProviderId>,
    /// The workspace member this task belongs to; `None` for root tasks.
    pub member: Option<Arc<WorkspaceMember>>,
    /// Everything else the source declared about the task. Populated by
    /// extractors that read a tool's own structured output (`mise tasks
    /// --json`); left at its default by file-parsing fallbacks.
    pub detail: TaskDetail,
}

pub(crate) type TaskDetail = runner_core::TaskDetail;

impl Task {
    /// Directory the task's config lives in: the member's directory, or
    /// `root`.
    pub(crate) fn dir<'a>(&'a self, root: &'a Path) -> &'a Path {
        self.member
            .as_ref()
            .map_or(root, |member| member.dir.as_path())
    }

    /// Directory the task executes in: the declared one when the source
    /// reports it, else [`Self::dir`].
    pub(crate) fn run_dir<'a>(&'a self, root: &'a Path) -> &'a Path {
        self.detail.dir.as_deref().unwrap_or_else(|| self.dir(root))
    }

    /// FQN scope segment: the member label, or `root`.
    pub(crate) fn scope(&self) -> &str {
        self.member
            .as_ref()
            .map_or("root", |member| member.label.as_str())
    }

    /// The spelling `run` accepts for this task: `member:name` for member
    /// tasks, the bare name otherwise.
    pub(crate) fn display_name(&self) -> Cow<'_, str> {
        self.member.as_ref().map_or_else(
            || Cow::Borrowed(self.name.as_str()),
            |member| Cow::Owned(format!("{}:{}", member.label, self.name)),
        )
    }

    /// Whether `other` lives in the same scope (root or the same member).
    pub(crate) fn same_scope(&self, other: &Self) -> bool {
        match (&self.member, &other.member) {
            (None, None) => true,
            (Some(a), Some(b)) => Arc::ptr_eq(a, b) || a.dir == b.dir,
            _ => false,
        }
    }
}

/// A package inside a workspace, discovered from the root's workspace
/// declaration.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct WorkspaceMember {
    /// Manifest `name`, or the directory name when the manifest has none.
    pub name: String,
    /// Directory relative to the workspace root, forward slashes.
    pub path: String,
    /// The scope token that addresses this member and no other: `name`
    /// when no sibling shares it, `path` otherwise.
    pub label: String,
    /// Absolute directory.
    pub dir: PathBuf,
}

impl WorkspaceMember {
    /// A member whose `label` is its name.
    #[cfg(test)]
    pub(crate) fn new(name: String, path: String, dir: PathBuf) -> Self {
        Self {
            label: name.clone(),
            name,
            path,
            dir,
        }
    }
}

/// Workspace declarations found at the workspace root and the members they
/// expand to, deduplicated by directory and sorted by path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Workspace {
    /// Directory holding the declarations.
    pub root: PathBuf,
    /// Every declaration found, e.g. `pnpm-workspace.yaml`.
    pub kinds: Vec<&'static str>,
    /// Members in path order.
    pub members: Vec<Arc<WorkspaceMember>>,
    /// The member containing the invocation directory, when runner was
    /// started inside one.
    pub current: Option<Arc<WorkspaceMember>>,
}

/// A runtime the root declares, the version it expects, and the version installed.
#[derive(Debug, Clone)]
pub(crate) struct RuntimeVersion {
    /// The runtime.
    pub runtime: ProviderId,
    /// The version the project declares.
    pub expected: Option<ExpectedVersion>,
    /// The installed version.
    pub current: Option<String>,
}

/// A declared runtime version and the file or field that declares it.
#[derive(Debug, Clone)]
pub(crate) struct ExpectedVersion {
    /// The version or range as written (e.g. `"20.11.0"`, `">=18"`).
    pub version: String,
    /// Where it is declared (e.g. `".nvmrc"`, `"package.json engines.node"`).
    pub source: String,
}

/// Non-fatal issue found while detecting project metadata or resolving a
/// package manager.
///
/// Carried as a typed variant so the diagnostic surface (`doctor --json`,
/// `--explain`) can attribute each warning to a chain step or detector,
/// and so future filtering (e.g. suppress just `PathProbeFallback`) is
/// trivial. The [`Display`] impl renders the same `"<source>: <detail>"`
/// shape every printer expects, so introducing a new variant doesn't
/// churn output sites.
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub(crate) enum DetectionWarning {
    /// A finding from provider observation or planning.
    Pipeline(runner_core::Warning),
    /// A task source whose tasks could not be read.
    Unread(runner_core::Unread),
    /// Manifest declaration (`packageManager` / `devEngines.packageManager`)
    /// disagrees with the detected lockfile. Declaration wins; the lockfile
    /// is likely stale.
    PmMismatch {
        /// The PM the manifest declared.
        declared: ProviderId,
        /// Which manifest field carried the declaration, `"packageManager"`
        /// or `"devEngines.packageManager"`. `&'static str` so it round-trips
        /// through `Display` and JSON unchanged.
        field: &'static str,
        /// The PM the lockfile points to.
        lockfile: ProviderId,
    },
    /// Resolver fell through to `PATH` probe because no declarations or
    /// lockfiles matched. Reports the picked binary plus any others that
    /// were also installed, so the user can spot drift between intent
    /// and environment.
    PathProbeFallback {
        /// Which PM the resolver picked.
        picked: ProviderId,
        /// Ecosystem the probe ran for (Node, Python, …).
        ecosystem: Ecosystem,
        /// Other PMs found on `PATH` that the resolver did not pick.
        others_available: Vec<ProviderId>,
    },
    /// An env-var override (`RUNNER_PM`, `RUNNER_RUNNER`) held a value
    /// that doesn't parse, and the command chose to report it instead
    /// of dying; `runner doctor` must be able to diagnose the broken
    /// environment it exists to diagnose. Strict commands still treat
    /// the same condition as a fatal error.
    InvalidEnvOverride {
        /// The variable that carried the value (`"RUNNER_PM"`).
        var: &'static str,
        /// The offending value, pre-sanitized for display (control
        /// chars escaped, truncated).
        raw: String,
        /// Rendered parse error, already source-prefixed.
        message: String,
    },
    /// `runner.toml` carries a key this build doesn't recognize, a typo, or
    /// a section/field added by a newer `runner`. Tolerated for forward
    /// compatibility: the key is ignored and the rest of the config still
    /// applies, so a config written by one version never bricks task
    /// dispatch under another. Surfaced as a warning so genuine typos stay
    /// visible instead of vanishing silently.
    UnknownConfigKey {
        /// Dotted path to the unrecognized key: `"github"` for an unknown
        /// section, `"chain.fast"` for an unknown field within a known one.
        path: String,
    },
    /// A `--runtime` / `RUNNER_RUNTIME` / `[runtime].js` override was set but
    /// the task that won selection dispatches through a tool with no JS
    /// runtime to choose. Surfaced so an explicit runtime is never a silent
    /// no-op.
    RuntimeNotApplied {
        /// The runtime the user asked for.
        runtime: ProviderId,
        /// Label of the task source that won selection (`"just"`, `"make"`).
        source: &'static str,
    },
}

impl DetectionWarning {
    /// Subsystem the warning came from, used as the prefix in both the
    /// human renderer (`warn: <source>: <detail>`) and the JSON shape
    /// (`{ "source": "...", "detail": "..." }`). Kept as `&'static str`
    /// so the JSON contract emitted by `doctor --json` stays byte-stable
    /// across the flat-struct → enum refactor.
    pub(crate) fn source(&self) -> &'static str {
        match self {
            Self::PmMismatch { declared, .. } => crate::provider::managed_sources()
                .into_iter()
                .find(|source| declared.dispatches().contains(source))
                .map_or("runner", Named::label),
            Self::PathProbeFallback { .. } => "resolver",
            Self::Pipeline(warning) => warning
                .provider
                .map_or("project", |id| runner_providers::REGISTRY.by_id(id).label),
            Self::Unread(unread) => runner_providers::REGISTRY.by_id(unread.provider).label,
            Self::InvalidEnvOverride { .. } => "env",
            Self::RuntimeNotApplied { .. } => "runtime",
            Self::UnknownConfigKey { .. } => "runner.toml",
        }
    }

    /// Human-readable detail line. Renders the variant-specific message
    /// without the `<source>:` prefix; pair with [`Self::source`] (or
    /// [`Display`]) to produce the full warning line.
    pub(crate) fn detail(&self) -> String {
        match self {
            Self::Pipeline(warning) => warning.message.clone(),
            Self::Unread(unread) => format!(
                "tasks in {} could not be read: {}",
                unread.scope.label(),
                unread.message
            ),
            Self::PmMismatch {
                declared,
                field,
                lockfile,
            } => format!(
                "{field} declares {} but the lockfile reflects {} (declaration wins); regenerate \
                 the lockfile to silence this",
                declared.label(),
                lockfile.label(),
            ),
            Self::PathProbeFallback {
                picked,
                ecosystem,
                others_available,
            } => {
                let eco = ecosystem.label();
                if others_available.is_empty() {
                    format!(
                        "no {eco} signals matched; using {} from PATH",
                        picked.label(),
                    )
                } else {
                    let others = others_available
                        .iter()
                        .map(|pm| pm.label())
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!(
                        "no {eco} signals matched; using {} from PATH (also available: {others})",
                        picked.label(),
                    )
                }
            }
            Self::InvalidEnvOverride { var, message, .. } => {
                format!("{var} is set but invalid and was ignored for this report: {message}")
            }
            Self::RuntimeNotApplied { runtime, source } => format!(
                "--runtime {} was not applied: this dispatches through {source}, which selects no \
                 JS runtime",
                runtime.label(),
            ),
            Self::UnknownConfigKey { path } => format!(
                "unknown key `{path}` ignored: it may be a typo or written by a newer runner. \
                 This build doesn't recognize it; the rest of the config still applies.",
            ),
        }
    }
}

impl std::fmt::Display for DetectionWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.source(), self.detail())
    }
}

/// Everything detected about the current project directory.
pub(crate) struct ProjectContext {
    /// Directory runner was invoked for (cwd or `--dir`). Local-file tokens
    /// and package-manager exec fallbacks resolve against it.
    pub cwd: PathBuf,
    /// Absolute path to the project root that was scanned: the workspace
    /// root when `cwd` sits inside a declared workspace, else `cwd`.
    pub root: PathBuf,
    /// All extracted tasks, sorted by source then name.
    pub tasks: Vec<Task>,
    /// Workspace declarations at the root and their expanded members.
    pub workspace: Option<Workspace>,
    /// Non-fatal detection issues surfaced to task-facing commands.
    pub warnings: Vec<DetectionWarning>,
    /// The core's observation and resolution of the tree under this
    /// invocation's policy.
    pub project: Result<runner_core::Project, Unobserved>,
}

/// An observation that failed, kept so every command that needs the project
/// reports the same failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Unobserved {
    pub kind: std::io::ErrorKind,
    pub message: String,
}

impl From<&Unobserved> for std::io::Error {
    fn from(unobserved: &Unobserved) -> Self {
        Self::new(unobserved.kind, unobserved.message.clone())
    }
}

impl Task {
    /// Bare-name precedence of this task's scope seen from `current`: the
    /// current member first, then the root, then every other member.
    pub(crate) fn scope_rank_from(&self, current: Option<&WorkspaceMember>) -> u8 {
        match (&self.member, current) {
            (Some(member), Some(current)) if member.dir == current.dir => 0,
            (None, _) => 1,
            (Some(_), _) => 2,
        }
    }

    /// The shortest token that runs this task from inside `current`: the
    /// bare name for the current member's tasks and for root tasks no
    /// current-member task shadows, `root:name` for a shadowed root task,
    /// `member:name` for other members.
    pub(crate) fn spelling_from<'a, 'b>(
        &'a self,
        current: Option<&WorkspaceMember>,
        siblings: impl IntoIterator<Item = &'b Self>,
    ) -> Cow<'a, str> {
        match self.scope_rank_from(current) {
            0 => Cow::Borrowed(self.name.as_str()),
            1 => {
                let shadowed = siblings
                    .into_iter()
                    .any(|task| task.name == self.name && task.scope_rank_from(current) == 0);
                if shadowed {
                    Cow::Owned(format!("root:{}", self.name))
                } else {
                    Cow::Borrowed(self.name.as_str())
                }
            }
            _ => self.display_name(),
        }
    }
}

impl ProjectContext {
    /// The workspace member the invocation directory sits in, if any.
    pub(crate) fn current_member(&self) -> Option<&Arc<WorkspaceMember>> {
        self.workspace.as_ref()?.current.as_ref()
    }

    pub(crate) fn scope_rank(&self, task: &Task) -> u8 {
        task.scope_rank_from(self.current_member().map(Arc::as_ref))
    }

    /// Whether a bare name spells this task from the invocation directory:
    /// root tasks and tasks of the current member.
    pub(crate) fn is_local(&self, task: &Task) -> bool {
        self.scope_rank(task) < 2
    }

    pub(crate) fn spelling<'a>(&self, task: &'a Task) -> Cow<'a, str> {
        task.spelling_from(self.current_member().map(Arc::as_ref), &self.tasks)
    }

    /// The package managers the root shows in its files, strongest first.
    pub(crate) fn package_managers(&self) -> Vec<ProviderId> {
        self.observed(runner_core::Kind::PACKAGE_MANAGER)
            .filter_map(crate::provider::package_manager)
            .collect()
    }

    /// The task runners the root shows in its files.
    pub(crate) fn task_runners(&self) -> Vec<ProviderId> {
        self.observed(runner_core::Kind::TASK_SOURCE)
            .filter_map(crate::provider::runner)
            .collect()
    }

    /// The runtimes the root declares, with an expected or installed version.
    pub(crate) fn runtime_versions(&self) -> Vec<RuntimeVersion> {
        self.project
            .iter()
            .flat_map(|project| &project.present)
            .filter(|present| present.scope == runner_core::Scope::Root)
            .filter(|present| {
                let kind = runner_providers::REGISTRY.by_id(present.provider).kind;
                kind.contains(runner_core::Kind::RUNTIME)
                    && !kind.intersects(runner_core::Kind::PACKAGE_MANAGER)
            })
            .map(|present| RuntimeVersion {
                runtime: present.provider,
                expected: expected_version(present),
                current: present.version.as_deref().map(|version| {
                    let version = version.trim();
                    version.strip_prefix('v').unwrap_or(version).to_owned()
                }),
            })
            .filter(|runtime| runtime.expected.is_some() || runtime.current.is_some())
            .collect()
    }

    /// Whether the project declares a workspace.
    pub(crate) const fn is_monorepo(&self) -> bool {
        self.workspace.is_some()
    }

    /// The labels of the providers of `kind` a root file shows, not only a
    /// `PATH` probe or the environment.
    fn observed(&self, kind: runner_core::Kind) -> impl Iterator<Item = &'static str> {
        let mut labels: Vec<&'static str> = Vec::new();
        for present in self.project.iter().flat_map(|project| &project.present) {
            let provider = runner_providers::REGISTRY.by_id(present.provider);
            let shown = present
                .because
                .first()
                .is_some_and(|evidence| evidence.weight <= runner_core::Weight::Configured);
            if present.scope == runner_core::Scope::Root
                && shown
                && provider.kind.intersects(kind)
                && !labels.contains(&provider.label)
            {
                labels.push(provider.label);
            }
        }
        labels.into_iter()
    }
}

/// The strongest version declaration behind `present`, and where it is written.
fn expected_version(present: &runner_core::Present) -> Option<ExpectedVersion> {
    let signals = runner_providers::REGISTRY.by_id(present.provider).signals;
    present.because.iter().find_map(|evidence| {
        let version = evidence.declared.as_ref()?.version()?;
        let file = evidence.at.file_name()?.to_string_lossy();
        let source = match signals.get(evidence.signal?.0)? {
            runner_core::Signal::ManifestField { path, .. } => format!("{file} {path}"),
            _ => file.into_owned(),
        };
        Some(ExpectedVersion {
            version: version.to_owned(),
            source,
        })
    })
}

/// The unified label vocabulary `[tasks].prefer` and `[tasks.overrides]`
/// accept: task runner labels, then package manager labels, then source
/// names, deduped. Single source of truth for both the resolver
/// (`resolver::policies::resolve_source_label`) and anything that needs to
/// advertise the same closed set (editor completion, the JSON Schema).
pub(crate) fn task_source_labels() -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    let mut push = |label: &'static str| {
        if !out.contains(&label) {
            out.push(label);
        }
    };
    for runner in crate::provider::runners() {
        push(runner.label());
    }
    for pm in crate::provider::package_managers() {
        push(pm.label());
    }
    for source in crate::provider::task_sources() {
        push(source.label());
    }
    out
}

/// Does `current` satisfy the `expected` version constraint?
///
/// `expected` accepts the node-semver range grammar found in `.nvmrc`,
/// `.node-version`, `.tool-versions`, and `package.json` `engines.node`:
/// comparator sets (`>=22.22.2`, `>=18 <21`), caret/tilde ranges
/// (`^20.11`, `~18.15`), `||` unions, hyphen ranges (`18 - 20`), and
/// wildcards (`20.x`). Evaluation is delegated to the `semver` crate
/// after normalizing node's grammar into the comma-separated comparator
/// form it parses.
///
/// Bare versions (`"20"`, `"20.11"`) keep prefix-at-segment-boundary
/// semantics: a `.nvmrc` saying `20.11` means "any 20.11.x", which is
/// narrower than the caret default the `semver` crate would apply.
///
/// Anything unevaluable (`lts/*`, malformed ranges, a non-version
/// `current`) falls back to the historical prefix match, so this never
/// panics and never rejects input it used to accept.
///
/// A prerelease `current` (e.g. `23.0.0-nightly`) only matches a
/// comparator that pins the same triple with a prerelease tag, the
/// `semver` crate's gate, mirroring node-semver's default behavior.
pub(crate) fn version_matches(expected: &str, current: &str) -> bool {
    let expected = expected.trim();
    let current = current.trim();

    if bare_version(expected) {
        return prefix_version_matches(expected, current);
    }
    range_matches(expected, current).unwrap_or_else(|| prefix_version_matches(expected, current))
}

/// The historical loose prefix match, kept as the fallback for inputs
/// the range path can't evaluate and as the primary semantics for bare
/// versions.
///
/// Strips leading range operators (`>=`, `~`, `^`, etc.) and checks
/// whether `current` starts with the cleaned `expected` value at a
/// segment boundary. A bare major version like `"20"` matches `"20.x.y"`.
fn prefix_version_matches(expected: &str, current: &str) -> bool {
    let after_ops = expected
        .trim()
        .trim_start_matches(">=")
        .trim_start_matches("<=")
        .trim_start_matches('>')
        .trim_start_matches('<')
        .trim_start_matches('=')
        .trim_start_matches('~')
        .trim_start_matches('^')
        .trim_start();
    let expected_clean = strip_v(after_ops).trim();

    current.starts_with(expected_clean)
        && current[expected_clean.len()..]
            .chars()
            .next()
            .is_none_or(|c| c == '.')
}

/// True when `s` is a plain version literal, optionally `v`-prefixed,
/// then nothing but ASCII digits and dots (`20`, `20.11`, `v20.11.0`).
/// Operators, wildcards, and named aliases (`lts/*`) all return false.
fn bare_version(s: &str) -> bool {
    let stripped = strip_v(s);
    !stripped.is_empty() && stripped.chars().all(|c| c.is_ascii_digit() || c == '.')
}

/// Strip a single leading `v` (`v18` → `18`) per nvm/Corepack convention.
fn strip_v(s: &str) -> &str {
    s.strip_prefix('v').unwrap_or(s)
}

/// Evaluate `expected` as a node-semver range against `current`.
///
/// Returns `None` when the outcome could not be determined, `current`
/// isn't a version, or no `||` group both parsed and matched while at
/// least one group was unparseable, so the caller can fall back to the
/// prefix match. A parsed-and-matching group wins immediately, letting
/// `">=18 || lts/*"` succeed on the evaluable half.
fn range_matches(expected: &str, current: &str) -> Option<bool> {
    let cur = parse_current_version(current)?;
    let mut any_unparseable = false;
    for group in expected.split("||") {
        let group = group.trim();
        if group.is_empty() {
            any_unparseable = true;
            continue;
        }
        let req = normalize_range_group(group)
            .and_then(|normalized| semver::VersionReq::parse(&normalized).ok());
        match req {
            Some(req) if req.matches(&cur) => return Some(true),
            Some(_) => {}
            None => any_unparseable = true,
        }
    }
    if any_unparseable { None } else { Some(false) }
}

/// Rewrite one `||`-free node-semver comparator group into the
/// comma-separated grammar `semver::VersionReq::parse` accepts.
///
/// Handles hyphen ranges (`18 - 20` → `>=18, <=20`; a partial upper
/// bound is already inclusive of its whole segment in the crate's
/// grammar), whitespace-separated AND comparators, operators detached
/// from their version (`>= 18`), and per-token `v` prefixes. Bare
/// digit-leading tokens get an `=` operator; the crate would otherwise
/// default them to caret, which is looser than node's exact-partial
/// semantics. Wildcard tokens (`*`, `x`) pass through untouched because
/// `=*` does not parse.
fn normalize_range_group(group: &str) -> Option<String> {
    let group = group.replace(',', " ");
    let tokens: Vec<&str> = group.split_whitespace().collect();
    if tokens.is_empty() {
        return None;
    }

    if let [low, "-", high] = tokens.as_slice() {
        return Some(format!(">={}, <={}", strip_v(low), strip_v(high)));
    }
    if tokens.contains(&"-") {
        return None;
    }

    let mut parts: Vec<String> = Vec::with_capacity(tokens.len());
    let mut iter = tokens.iter();
    while let Some(token) = iter.next() {
        let (op, rest) = split_operator(token);
        if op.is_empty() {
            let rest = strip_v(rest);
            if rest.starts_with(|c: char| c.is_ascii_digit()) {
                parts.push(format!("={rest}"));
            } else {
                parts.push(rest.to_string());
            }
        } else if rest.is_empty() {
            let version = iter.next()?;
            parts.push(format!("{op}{}", strip_v(version)));
        } else {
            parts.push(format!("{op}{}", strip_v(rest)));
        }
    }
    Some(parts.join(", "))
}

/// Split a leading range operator off a comparator token. Returns
/// `(op, rest)` with `op` ∈ {`>=`, `<=`, `>`, `<`, `=`, `~`, `^`, ``""``}.
fn split_operator(token: &str) -> (&str, &str) {
    for op in [">=", "<=", ">", "<", "=", "~", "^"] {
        if let Some(rest) = token.strip_prefix(op) {
            return (op, rest);
        }
    }
    ("", token)
}

/// Parse `current` (a `node --version`-style string with the `v`
/// already stripped by detection) into a full [`semver::Version`],
/// padding bare `major`/`major.minor` forms to a triple.
fn parse_current_version(current: &str) -> Option<semver::Version> {
    let padded = match current.split('.').count() {
        1 => format!("{current}.0.0"),
        2 => format!("{current}.0"),
        _ => current.to_string(),
    };
    semver::Version::parse(&padded).ok()
}

#[cfg(test)]
mod tests {
    use super::{DetectionWarning, TaskDetail, range_matches, version_matches};

    #[test]
    fn serialized_labels_match_label_methods() {
        use crate::resolver::{FallbackPolicy, MismatchPolicy, ScriptPolicy};

        fn json_str<T: serde::Serialize>(value: T) -> String {
            serde_json::to_value(value)
                .expect("enum should serialize")
                .as_str()
                .expect("enum should serialize as a string")
                .to_string()
        }

        for fallback in FallbackPolicy::ALL {
            assert_eq!(json_str(fallback), fallback.label());
        }
        for mismatch in MismatchPolicy::ALL {
            assert_eq!(json_str(mismatch), mismatch.label());
        }
        for script in ScriptPolicy::SETTABLE {
            assert_eq!(Some(json_str(script).as_str()), script.label());
        }
        // Default has no user-settable label; the report surface still
        // needs a stable spelling.
        assert_eq!(json_str(ScriptPolicy::Default), "default");
    }

    /// Every printed spelling must resolve to the member it names.
    #[test]
    fn colliding_member_names_spell_tasks_by_path() {
        use std::path::PathBuf;
        use std::sync::Arc;

        use super::{ProviderId, Task, Workspace, WorkspaceMember};

        let mut members = vec![
            WorkspaceMember::new(
                "web".to_string(),
                "apps/web".to_string(),
                PathBuf::from("/ws/apps/web"),
            ),
            WorkspaceMember::new(
                "api".to_string(),
                "services/api".to_string(),
                PathBuf::from("/ws/services/api"),
            ),
            WorkspaceMember::new(
                "web".to_string(),
                "tools/web".to_string(),
                PathBuf::from("/ws/tools/web"),
            ),
        ];
        members[0].label = "apps/web".to_string();
        members[2].label = "tools/web".to_string();
        let workspace = Workspace {
            root: PathBuf::from("/ws"),
            kinds: vec!["package.json workspaces"],
            members: members.into_iter().map(Arc::new).collect(),
            current: None,
        };
        let tasks: Vec<Task> = workspace
            .members
            .iter()
            .map(|member| Task {
                name: "build".to_string(),
                source: ProviderId::PackageJson,
                run_target: None,
                description: None,
                alias_of: None,
                passthrough_to: None,
                detail: TaskDetail::default(),
                member: Some(Arc::clone(member)),
            })
            .collect();

        let spellings: Vec<String> = tasks
            .iter()
            .map(|task| task.spelling_from(None, &tasks).into_owned())
            .collect();
        assert_eq!(
            spellings,
            vec!["apps/web:build", "api:build", "tools/web:build"],
        );
    }

    #[test]
    fn dotted_versions_match_segment_boundaries_only() {
        assert!(version_matches("20.11", "20.11.0"));
        assert!(!version_matches("20.11", "20.110.0"));
    }

    #[test]
    fn gte_range_matches_higher_versions() {
        // Regression: ">=22.22.2" used to prefix-match as "=22.22.2",
        // warning on 22.22.3 and 25.9.0, which both satisfy the range.
        assert!(version_matches(">=22.22.2", "22.22.3"));
        assert!(version_matches(">=22.22.2", "25.9.0"));
        assert!(!version_matches(">=22.22.2", "22.22.1"));
    }

    #[test]
    fn operator_with_space_before_version() {
        assert!(version_matches(">= 18", "20.0.0"));
        assert!(!version_matches(">= 18", "17.9.0"));
    }

    #[test]
    fn partial_comparator_bounds() {
        assert!(version_matches(">=18", "18.0.0"));
        // node desugars ">22" to ">=23.0.0": 22.x never qualifies.
        assert!(!version_matches(">22", "22.5.0"));
        assert!(version_matches(">22", "23.0.0"));
        assert!(version_matches("<21", "20.99.0"));
        assert!(!version_matches("<21", "21.0.0"));
        assert!(version_matches("<=20", "20.99.0"));
    }

    #[test]
    fn caret_ranges() {
        // The case bare-prefix semantics must reject but caret accepts.
        assert!(version_matches("^20.11", "20.12.0"));
        assert!(!version_matches("^20.11", "20.10.9"));
        assert!(!version_matches("^20.11", "21.0.0"));
        assert!(version_matches("^0.3", "0.3.9"));
        assert!(!version_matches("^0.3", "0.4.0"));
    }

    #[test]
    fn tilde_ranges() {
        assert!(version_matches("~18.15", "18.15.7"));
        assert!(!version_matches("~18.15", "18.16.0"));
        assert!(version_matches("~18.15.0", "18.15.3"));
    }

    #[test]
    fn space_separated_and_conjunction() {
        assert!(version_matches(">=18 <21", "20.5.1"));
        assert!(!version_matches(">=18 <21", "21.0.0"));
        assert!(!version_matches(">=18 <21", "17.0.0"));
    }

    #[test]
    fn or_unions() {
        assert!(version_matches("18||20", "20.4.2"));
        assert!(!version_matches("18||20", "19.0.0"));
        assert!(version_matches(">=18 <19 || >=20", "18.5.0"));
        assert!(!version_matches(">=18 <19 || >=20", "19.5.0"));
        assert!(version_matches(">=18 <19 || >=20", "25.9.0"));
    }

    #[test]
    fn hyphen_ranges() {
        assert!(version_matches("18 - 20", "19.0.0"));
        // Inclusive partial upper bound: node treats "- 20" as "<21".
        assert!(version_matches("18 - 20", "20.9.9"));
        assert!(!version_matches("18 - 20", "21.0.0"));
        assert!(!version_matches("18 - 20", "17.9.9"));
    }

    #[test]
    fn wildcard_ranges() {
        assert!(version_matches("20.x", "20.5.1"));
        assert!(!version_matches("20.x", "21.0.0"));
        assert!(version_matches("20.*", "20.0.0"));
        assert!(version_matches("*", "99.0.0"));
    }

    #[test]
    fn bare_versions_keep_prefix_semantics() {
        // Regression guard for the caret trap: the semver crate would
        // read a bare "20.11" as "^20.11" and accept 20.12.
        assert!(!version_matches("20.11", "20.12.0"));
        assert!(version_matches("20", "20.11.0"));
        assert!(!version_matches("2", "20.11.0"));
        assert!(version_matches("v20", "20.1.0"));
        assert!(version_matches("20.11.0", "20.11.0"));
    }

    #[test]
    fn exact_operator_partial_equality() {
        assert!(version_matches("=20.11", "20.11.5"));
        assert!(!version_matches("=20.11", "20.12.0"));
    }

    #[test]
    fn operator_with_v_prefix() {
        assert!(version_matches(">=v18", "18.0.0"));
    }

    #[test]
    fn an_unparseable_constraint_cannot_be_evaluated() {
        assert_eq!(range_matches("lts/*", "22.0.0"), None);
        assert_eq!(range_matches("lts/jod", "22.0.0"), None);
        assert_eq!(range_matches("", "20.0.0"), None);
    }

    #[test]
    fn unparseable_or_group_does_not_block_parsed_match() {
        assert!(version_matches(">=18 || lts/*", "20.0.0"));
    }

    #[test]
    fn an_unparseable_found_version_cannot_be_evaluated() {
        assert_eq!(range_matches(">=18", "not-a-version"), None);
    }

    #[test]
    fn a_prerelease_tagged_found_version_is_never_matched_by_prefix() {
        assert_eq!(range_matches("=20.11", "20.11.beta"), None);
        assert_eq!(range_matches("=20.11", "20.12.beta"), None);
        assert_eq!(range_matches(">= v18", "18.unknown"), None);
    }

    #[test]
    fn detection_warning_can_be_hashed() {
        use std::collections::HashSet;

        let a = DetectionWarning::UnknownConfigKey { path: "a".into() };
        let b = DetectionWarning::UnknownConfigKey { path: "a".into() };
        let c = DetectionWarning::UnknownConfigKey { path: "c".into() };

        let mut set = HashSet::new();
        set.insert(a);
        set.insert(b);
        set.insert(c);

        assert_eq!(set.len(), 2, "equal variants should dedup");
    }
}
