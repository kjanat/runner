# runner core design

runner observes a directory, decides which tools are present and what they
can do, and turns one request into one command it can explain.

Every type, trait and rule in this document is derived from that sentence.
Anything that cannot be derived from it does not belong in the core.

## 1. Vocabulary

| Word       | Meaning                                                                                                      |
| ---------- | ------------------------------------------------------------------------------------------------------------ |
| Tree       | The directory runner was invoked for, its ancestors up to the project root, and its workspace members.       |
| Provider   | One tool runner knows about: a package manager, a task source, a runtime, or a tool manager. One value each. |
| Signal     | Something a provider tells the core to look for: a file, a manifest field, a lockfile, an env var.           |
| Evidence   | A signal that was found, with where it was found, which scope it belongs to, and how strong it is.           |
| Scope      | The workspace member a piece of evidence or a task belongs to, or the root.                                  |
| Present    | A provider with enough evidence to count as part of the project, plus its resolved version and bin dirs.     |
| Capability | Something a present provider can do, with the parameters the core needs to drive it.                         |
| Op         | What the user asked for: install, run a task, exec a name, run a file, test, clean, health.                  |
| Policy     | The override chain, resolved once per invocation: CLI, env, `runner.toml`, manifest, lockfile, probe.        |
| Plan       | One command, the provider that owns it, its trust and reach, and the evidence and policy that produced it.   |
| Scheme     | An ecosystem's version grammar and comparison rules.                                                         |

## 2. Pipeline

```text
observe(tree)            -> Vec<Evidence>
resolve(evidence, policy) -> Project { present: Vec<Present>, tasks: Vec<Task>, warnings }
plan(project, op, policy) -> Result<Plan, Refusal>
execute(plan)             -> ExitStatus
explain(plan | project)   -> Report
```

Each subcommand is one of these stages exposed.

| Subcommand                    | Stops after | Notes                                               |
| ----------------------------- | ----------- | --------------------------------------------------- |
| `info`, `list`, `completions` | resolve     | Render the project.                                 |
| `doctor`                      | resolve     | Render the project plus every provider's health op. |
| `why`, any `--explain`        | plan        | Render the plan without executing it.               |
| `run`, `install`, `clean`     | execute     | Render the plan's arrow, then spawn.                |
| `config`, `schema`, `lsp`     | none        | Read the declaration tables directly.               |

Explanation is the plan rendered. No subcommand rebuilds a decision by a
second code path.

## 3. Articles

Each article is a rule the whole tree obeys and names the test that enforces
it.

1. **Providers declare, the core decides.** A provider never reads policy. The
   core never branches on a provider id. Enforced by a lint test that greps
   `src/core` for `ProviderId::` in match arms and fails on any hit outside
   the registry.
2. **One resolver, every ecosystem, one override chain.** `resolve` takes an
   ecosystem argument. There is no second resolver. Enforced by a test that
   resolves each ecosystem through the same function with the same six
   layers and checks the winning layer is reported identically.
3. **Evidence before action.** A plan carries the evidence that produced it.
   A plan with an empty `because` is a bug. Enforced by a debug assertion in
   `execute` and a test over every op for every provider.
4. **Local before network.** Rungs of the run cascade are sorted by `Reach`.
   A rung that can fetch never precedes a rung that cannot. Enforced by a
   test that asserts the cascade table is sorted.
5. **Every setting declared once.** A config field, its env var, its CLI
   flag, its schema entry, its completion and its doc line come from one row
   of one table. Enforced by the existing drift guards, extended to provider
   labels and capability names.
6. **Observation is read only.** Reading a file and asking a tool a read-only
   question are both observation. Asking a tool to change anything is
   execution and only a plan does that. Enforced by review, and by
   `observe` taking `&Tree` with no access to `Policy` or `Op`.
7. **Scope is a first class fact.** Every evidence and every task carries its
   scope. A filter that drops tasks by path is a bug. Enforced by a monorepo
   fixture that asserts root tasks are visible from a member and member
   tasks are visible from the root with their scope attached.
8. **Host tools come from the host.** A plan with `Trust::Host` never has
   project bin dirs on its `PATH`. Enforced by a test that commits a fake
   `node_modules/.bin/mise` and asserts the install plan does not resolve to
   it.
9. **No shell.** A plan is an argv. runner never renders a command line for a
   shell to parse. Tools that take a script body receive it as one argument.
   Enforced by a test that greps plans for `sh -c`, `cmd /c` and
   `powershell -Command`.

## 4. Core types

### 4.1 Identity

```rust
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ProviderId {
    Npm,
    Yarn,
    Pnpm,
    Bun,
    Deno,
    Cargo,
    Go,
    Uv,
    Poetry,
    Pipenv,
    Bundler,
    Composer,
    Turbo,
    Nx,
    Make,
    Just,
    Task,
    Mise,
    Bacon,
    Volta,
    PackageJson,
    Pyproject, // …
}

pub enum Ecosystem {
    Node,
    Deno,
    Python,
    Rust,
    Go,
    Ruby,
    Php,
    Any,
}

bitflags! { pub struct Kind: u8 {
    const PACKAGE_MANAGER = 1; const TASK_SOURCE = 2; const RUNTIME = 4; const TOOL_MANAGER = 8;
} }
```

`ProviderId` is an index into the registry and carries no behaviour. Deno is
`PACKAGE_MANAGER | TASK_SOURCE | RUNTIME`. Mise is
`TASK_SOURCE | TOOL_MANAGER`. Kinds are a set because the tools are.

`Ecosystem::Any` is for task runners that do not belong to a language
(`make`, `just`, `mise`).

### 4.2 Signals and evidence

```rust
pub enum Signal {
    File(&'static str),        // present in the scope directory
    FileUpwards(&'static str), // present in the scope directory or an ancestor
    Lockfile(&'static str),    // a File that also pins the provider
    ManifestField {
        file: &'static str,
        path: &'static str,
        parse: fn(&Value) -> Option<Declared>,
    },
    EnvVar(&'static str), // set in runner's own environment
    Probe(&'static str),  // executable on PATH, checked last
    Ask(fn(&Path) -> io::Result<Vec<Evidence>>), // a read-only subprocess, see article 6
}

pub enum Weight {
    Declared,
    Locked,
    Configured,
    Present,
    Probed,
}

pub struct Evidence {
    pub provider: ProviderId,
    pub signal: SignalId,
    pub at: PathBuf,
    pub scope: Scope,
    pub weight: Weight,
    pub declared: Option<Declared>, // a version constraint or a named alternative
}

pub enum Scope {
    Root,
    Member { name: String, dir: PathBuf },
}
```

`Weight` is ordered. `Declared` (a manifest says so) beats `Locked` (a
lockfile says so) beats `Configured` (a tool config exists) beats `Present`
(a directory such as `.venv` exists) beats `Probed` (it is on `PATH`). The
resolver sorts on weight within an ecosystem after policy has had its say.

`Signal::Ask` exists for tools that own their own truth, such as
`mise tasks --json`. It receives a directory and nothing else. It cannot see
policy or the op, so it cannot be turned into execution.

### 4.3 Provider

```rust
pub struct Provider {
    pub id: ProviderId,
    pub label: &'static str,
    pub aliases: &'static [&'static str],
    pub ecosystem: Ecosystem,
    pub kind: Kind,
    pub program: &'static str, // the executable name, probed with PATHEXT on Windows
    pub signals: &'static [Signal],
    pub writes: &'static [&'static str], // install dirs this provider materialises
    pub caps: Capabilities,
    pub tasks: Option<fn(&Present, &Tree) -> Result<Vec<Task>>>,
    pub version: Option<fn(&Present) -> Result<String>>,
    pub hooks: Hooks,
}

pub struct Hooks {
    pub before_plan: Option<fn(&Present, &Op, &mut Vec<Warning>)>,
    pub after_observe: Option<fn(&Tree, &[Evidence]) -> Vec<Evidence>>,
}
```

A provider is a value in a `static REGISTRY: &[Provider]`. Most fields are
data. The three function pointers are the only places provider code runs,
and each has a narrow reason:

- `tasks` when the task format is the tool's own (justfile parsing, `mise
  tasks --json`, `[[bin]]` in `Cargo.toml`, `cmd/<name>` in Go).
- `version` when `<program> --version` output needs a tool-specific parse.
- `hooks.before_plan` for a warning the core cannot know, such as bun and
  pnpm refusing to re-enable dependency build scripts without a manifest
  allowlist.
- `hooks.after_observe` for evidence derived from other evidence, such as
  yarn classic versus berry from the `packageManager` field.

A provider that needs a fourth function is telling you the core is missing a
capability parameter. Add the parameter.

### 4.4 Capabilities

```rust
pub struct Capabilities {
    pub install: Option<InstallCap>,
    pub run_task: Option<RunTaskCap>,
    pub exec: Option<ExecCap>,
    pub run_file: Option<RunFileCap>,
    pub test: Option<TestCap>,
    pub bins: Option<BinsCap>,
    pub clean: Option<CleanCap>,
    pub workspaces: Option<WorkspaceCap>,
    pub health: Option<HealthCap>,
    pub usage: Option<UsageCap>,
    pub operations: &'static [&'static str],
    pub quiet: QuietSupport,
}
```

Each capability holds an argv template and the parameters policy can turn on.

```rust
pub struct InstallCap {
    pub argv: Template,                         // ["install"]
    pub frozen: Frozen, /* Flag("--frozen-lockfile") | Subcommand("ci") | Env("UV_FROZEN","1") | Unsupported */
    pub scripts: ScriptSupport, /* deny: Flag("--ignore-scripts"), allow: Flag("--no-ignore-scripts") | Env(..) | Default | Unsupported */
    pub locked_only_with: Option<&'static str>, // mise: `--locked` needs a lockfile present
}

pub struct RunTaskCap {
    pub argv: Template,                 // ["run", Task, Sep("--"), Args]
    pub sources: &'static [ProviderId], // task sources this provider can dispatch
}

pub struct ExecCap {
    pub program: Option<&'static str>, // npx is not npm, uvx is not uv
    pub argv: Template,                // ["exec", Name, Args] or ["x", Name, Args]
    pub reach: Reach, // Network for npx, bun x, uvx, deno x; Local for `yarn run` shapes
    pub accepts: NameShape, // Bare | PathLike | Versioned, so `go run` only takes module paths
}

pub struct RunFileCap {
    pub program: Option<&'static str>,
    pub extensions: &'static [&'static str],
    pub argv: Template,
}

pub struct TestCap {
    pub program: Option<&'static str>, // npm's test runner is `node --test`
    pub argv: Template,                // ["test", Args]
    pub discovery: Discovery, /* Tool (the runner finds its own files) | Files { patterns } | Detect(fn) */
}

pub struct BinsCap {
    pub dirs: BinDirs,
} // Static(&["node_modules/.bin"]) | Ask(fn(&Path) -> Vec<PathBuf>)
pub struct CleanCap {
    pub dirs: &'static [&'static str],
}
pub struct WorkspaceCap {
    pub members: fn(&Tree) -> Result<Vec<Scope>>,
}
pub struct HealthCap {
    pub argv: Template,
    pub parse: fn(&[u8]) -> Health,
}
pub struct UsageCap {
    pub spec: fn(&Present, &Task) -> Result<Option<UsageSpec>>,
}

pub struct QuietSupport {
    pub levels: [Option<Template>; 4],
    pub stream: Option<Template>,
    pub limitation: &'static str, // why the ladder stops where it does, for the Clamp
}
```

The `Template` type is what keeps the ordering quirks out of provider
functions:

```rust
pub enum Piece {
    Lit(&'static str),
    Task,
    Name,
    Args,
    Sep(&'static str),
    Quiet,
    Frozen,
    Scripts,
    File,
    Files, // what Discovery::Files found
    Op,    // the tool-manager operation
}
pub struct Template(pub &'static [Piece]);
```

mise wants `--quiet` before `run`, cargo wants `-q` after the subcommand,
npm wants `--silent` anywhere. Each is a different position of `Quiet` in
the template. The core renders the template, drops `Sep` when `Args` is
empty, and drops any parameter piece policy did not turn on.

`Discovery::Files` is what `node --test` needs and `bun test` does not.
`Discovery::Detect` is what Python needs, where the runner is itself a
finding: pytest, nose2, ward, Django, tox, nox, unittest, in that order,
each with its own evidence.

### 4.5 Ops and policy

```rust
pub enum Op<'a> {
    Install { operations: &'a [String] },
    Run { task: &'a Task, args: &'a [String] },
    Exec { name: &'a str, args: &'a [String] },
    RunFile { file: &'a Path, args: &'a [String] },
    Test { args: &'a [String] },
    Clean,
    Health,
}

pub enum Layer {
    Cli,
    Env,
    ConfigFile(PathBuf),
    Manifest(PathBuf),
    Lockfile(PathBuf),
    Probe,
}

pub struct Policy {
    pub pm: PerEcosystem<Choice>, // `--pm`, `RUNNER_PM`, `[pm].<eco>`
    pub runner: Option<Choice>,   // `--runner`, `RUNNER_RUNNER`, `[tasks].prefer`
    pub runtime: Option<Choice>,  // `--runtime`, `RUNNER_RUNTIME`, `[runtime].js`
    pub frozen: bool,
    pub scripts: ScriptPolicy,
    pub reach: ReachPolicy, // Ask | Allow | Local
    pub verbosity: Verbosity,
    pub env: EnvLayers,                              // project, per tool, per task
    pub tool_ops: BTreeMap<ProviderId, Vec<String>>, // `[tools.<name>].install`
    pub trust: TrustPolicy,                          // see section 6
}

pub struct Choice {
    pub id: ProviderId,
    pub from: Layer,
}
```

Policy is built once from the declaration table and handed to `resolve` and
`plan`. `observe` never sees it.

The file that feeds it is specified by issue 123 (`runner.toml` v2): config
holds only what observation cannot conclude, every key reads as a sentence a
user would say, and a line detection would have concluded anyway is a lint
error. Three tables: `[tools]` for vetoes and tie-breaks, `[tasks]` for
decoration and composition, `[defaults]` for taste. Four additions the core
needs that the issue leaves out:

- `fetch = "ask" | "allow" | "local"`, a trust decision.
- `env` at project, tool and task scope.
- `[tools.<name>].install`, the operations a tool manager runs.
- `quiet = true` on a task, translated by the provider table, in place of
  `args = ["-q"]`.

### 4.6 Plan

```rust
pub struct Plan {
    pub provider: Option<ProviderId>, // None for a file on disk or a binary on a search path
    pub found: Option<PathBuf>,       // what the path, file, dep, bins or host rung found
    pub argv: Vec<OsString>,          // program first, never a shell string
    pub cwd: PathBuf,
    pub env: Vec<(OsString, OsString)>,
    pub path_prepend: Vec<PathBuf>, // empty when trust is Host
    pub trust: Trust,
    pub reach: Reach,
    pub clamps: Vec<Clamp>, // requested vs granted, for verbosity and scripts
    pub because: Vec<Evidence>,
    pub decided_by: Vec<Layer>,
    pub scope: Scope,
}

pub enum Trust {
    Host,
    Project,
}
pub enum Reach {
    Local,
    Network,
}

pub enum Refusal {
    NotFound {
        name: String,
        tried: Vec<Rung>,
    },
    Declined {
        name: String,
        rung: Rung,
    },
    NoCapability {
        provider: ProviderId,
        op: &'static str,
    },
    Ambiguous {
        candidates: Vec<(ProviderId, Scope)>,
    },
    Unsafe(Unsafe),
}
```

A `Plan` is complete. `execute` adds nothing and decides nothing. That is
what makes `why`, `--explain`, `doctor` and the arrow line agree, because
they all print the same struct.

`Trust` is set by the op. Toolchain installs and health checks are `Host`.
Everything the project asked for is `Project`. See section 6.

### 4.7 The run cascade

```rust
pub struct Rung {
    pub name: &'static str,
    pub needs: Need,
    pub reach: Reach,
}

pub static CASCADE: &[Rung] = &[
    Rung {
        name: "builtin",
        needs: Need::BareVerb,
        reach: Reach::Local,
    },
    Rung {
        name: "path",
        needs: Need::ExplicitPath,
        reach: Reach::Local,
    },
    Rung {
        name: "task",
        needs: Need::Task,
        reach: Reach::Local,
    },
    Rung {
        name: "file",
        needs: Need::RelativeFile,
        reach: Reach::Local,
    },
    Rung {
        name: "dep",
        needs: Need::InstalledDep,
        reach: Reach::Local,
    },
    Rung {
        name: "test",
        needs: Need::Cap(Cap::Test),
        reach: Reach::Local,
    },
    Rung {
        name: "bins",
        needs: Need::ProjectBins,
        reach: Reach::Local,
    },
    Rung {
        name: "host",
        needs: Need::HostPath,
        reach: Reach::Local,
    },
    Rung {
        name: "manager",
        needs: Need::ToolManagerExec,
        reach: Reach::Network,
    },
    Rung {
        name: "exec",
        needs: Need::Cap(Cap::Exec),
        reach: Reach::Network,
    },
];
```

The table is data so article 4 can be a test. The prompt lives in
`plan`, keyed on `reach` and `policy.reach`, and it is the same prompt for
every network rung.

### 4.8 Versions

```rust
pub trait Scheme {
    type Version: Ord;
    type Constraint;
    fn version(s: &str) -> Result<Self::Version, ParseError>;
    fn constraint(s: &str) -> Result<Self::Constraint, ParseError>;
    fn satisfies(v: &Self::Version, c: &Self::Constraint) -> bool;
}

pub enum Check {
    Satisfied,
    Violated { declared: String, found: String },
    Unknown { reason: String },
}

pub fn check<S: Scheme>(declared: &str, found: &str) -> Check;
```

Schemes are per ecosystem: `NodeSemver` (npm ranges), `CargoSemver`
(caret default, comma AND), `GoModule` (`v` prefix, `+incompatible`,
pseudo-versions), `Pep440`, `RubyGems` (`~>`, letter segments),
`ComposerSemver` (stability flags, branch aliases). A tool manager declares
its own for its tool table, since `node = "lts"` in `mise.toml` is none of
the above.

The scheme is a type parameter, so a Go version against a PEP 440 constraint
does not compile. `Unknown` is a real answer and `doctor` reports it as
"cannot evaluate" with the reason.

### 4.9 Tasks

```rust
pub struct Task {
    pub name: String,
    pub source: ProviderId,
    pub scope: Scope,
    pub target: Option<String>, // `go run ./cmd/name`, `cargo run --bin name`
    pub description: Option<String>,
    pub alias_of: Option<String>,
    pub forwards_to: Option<ProviderId>, // `"build": "just build"`
    pub detail: TaskDetail,              // depends, env, tools, usage, sources, outputs, timeout
}
```

`scope` replaces the current path filter. A task from the root seen from a
member is still a task, with `Scope::Root`. Precedence is computed from
scope by the core, once, and rendered by `list` and `why` from the same
field.

## 5. Provider anatomy

pnpm is pure data:

```rust
Provider {
    id: Pnpm, label: "pnpm", aliases: &[], ecosystem: Node, kind: PACKAGE_MANAGER, program: "pnpm",
    signals: &[Lockfile("pnpm-lock.yaml"), ManifestField { file: "package.json", path: "packageManager", parse: node::package_manager },
               ManifestField { file: "package.json", path: "devEngines.packageManager", parse: node::dev_engines }, Probe("pnpm")],
    writes: &["node_modules"],
    caps: Capabilities {
        install: Some(InstallCap { argv: t!["install"], frozen: Frozen::Flag("--frozen-lockfile"),
                                   scripts: ScriptSupport { deny: Flag("--ignore-scripts"), allow: Warn("needs onlyBuiltDependencies") }, .. }),
        run_task: Some(RunTaskCap { argv: t!["run", Task, Sep("--"), Args], sources: &[PackageJson] }),
        exec: Some(ExecCap { argv: t!["exec", Name, Args], reach: Network, accepts: Bare | Versioned }),
        run_file: None,
        test: Some(TestCap { argv: t![Lit("node"), "--test", Args], discovery: Files(&["test.{js,ts,…}", "*.test.{js,ts,…}"]) }),
        bins: Some(BinsCap { dirs: Static(&["node_modules/.bin"]) }),
        clean: Some(CleanCap { dirs: &["node_modules"] }),
        workspaces: Some(WorkspaceCap { members: node::workspace_members }),
        quiet: QuietSupport { levels: [None, Some(t!["--silent"]), Some(t!["--silent"]), Some(t!["--silent"])], stream: None },
        ..Capabilities::NONE
    },
    tasks: None, version: None, hooks: Hooks::NONE,
}
```

mise carries the functions the core cannot express as data:

```rust
Provider {
    id: Mise, label: "mise", aliases: &["rtx"], ecosystem: Any, kind: TASK_SOURCE | TOOL_MANAGER, program: "mise",
    signals: &[FileUpwards("mise.toml"), FileUpwards(".mise.toml"), FileUpwards("mise.local.toml"),
               FileUpwards(".config/mise/config.toml"), EnvVar("MISE_SHELL"), Ask(mise::tasks_json)],
    writes: &[],
    caps: Capabilities {
        run_task: Some(RunTaskCap { argv: t![Quiet, "run", Task, Sep("--"), Args], sources: &[Mise] }),
        exec: Some(ExecCap { argv: t!["exec", Sep("--"), Name, Args], reach: Network, accepts: Bare }),
        bins: Some(BinsCap { dirs: Ask(mise::bin_paths) }),
        health: Some(HealthCap { argv: t!["tasks", "validate", "--json"], parse: mise::health }),
        usage: Some(UsageCap { spec: mise::usage_spec }),
        operations: &["install", "bootstrap"],
        install: Some(InstallCap { argv: t![Op], frozen: Frozen::Flag("--locked"), locked_only_with: Some("mise.lock"), .. }),
        quiet: QuietSupport { levels: [None, Some(t!["--quiet"]), Some(t!["--quiet"]), Some(t!["--quiet"])], stream: None },
        ..Capabilities::NONE
    },
    tasks: Some(mise::tasks), version: None, hooks: Hooks::NONE,
}
```

Four functions, each answering a question only mise can answer. Nothing in
`src/core` names mise.

## 6. Security and variability

### 6.1 Trust boundaries

Two parties can put bytes in front of runner, and they are not the same
party.

| Input                              | Controlled by | Trust   |
| ---------------------------------- | ------------- | ------- |
| CLI flags, `RUNNER_*` env          | user          | full    |
| `~/.config/runner/runner.toml`     | user          | full    |
| `runner.toml` in the repository    | repository    | project |
| manifests, lockfiles, tool configs | repository    | project |
| `node_modules/.bin`, `.venv/bin`   | repository    | project |
| tool manager bin dirs              | tool manager  | project |

Rules that follow:

1. **Host trust for host ops.** `Trust::Host` plans get the user's `PATH`
   only. The toolchain install, health checks and any subprocess used in
   observation run as `Host`. A committed `node_modules/.bin/mise` cannot
   become the mise that installs the toolchain.
2. **Env layers from project trust cannot replace the loader.** `[env]` in a
   repository `runner.toml` may add variables. It may not set `PATH`,
   `LD_PRELOAD`, `LD_LIBRARY_PATH`, `DYLD_*`, `NODE_OPTIONS`,
   `PYTHONSTARTUP`, `RUBYOPT`, `PERL5OPT`, `GOFLAGS`, `CARGO_BUILD_RUSTC`
   or any variable a tool documents as a code-loading hook. The core holds
   the denylist. A user-trust config may set anything.
3. **Network is a flag, and the flag has a policy.** Every rung and every
   capability declares `Reach`. `Reach::Network` plans consult
   `policy.reach`: `Ask` prompts on a terminal, `Allow` proceeds, `Local`
   refuses. `RUNNER_REACH=local` makes CI deterministic.
4. **Lifecycle scripts are a declared parameter.** `ScriptPolicy` maps to
   each provider's `ScriptSupport`. When a provider cannot honour the
   request the plan records a `Clamp` and the arrow says so.
5. **Tool manager trust is the tool manager's.** runner never runs
   `mise trust`. An untrusted config is evidence with a warning and
   `doctor` relays the tool's own message.
6. **Deno permissions are the project's.** runner never adds `-A` or any
   `--allow-*`. The plan passes what `deno.json` or the task declares.
7. **Symlinks do not escape.** Scope assignment canonicalises paths before
   comparing them to the root, so a task file symlinked in from outside is
   `Scope::Root` only when it really resolves under the root.
8. **Names are validated by shape.** `ExecCap::accepts` says whether a
   provider takes bare names, path-like names or versioned names. A name
   with a path separator never reaches `npx`, and a bare name never reaches
   `go run`.
9. **Explanations print names, never values.** `why` and `doctor` render
   env layer keys and where they came from. Values stay out of every
   report.

### 6.2 What varies per ecosystem

The table is the checklist for a new provider. If a row has no matching
capability parameter, the core is missing one.

| Axis           | Node                                | Deno              | Python                                   | Rust                 | Go                 | Ruby               | PHP             |
| -------------- | ----------------------------------- | ----------------- | ---------------------------------------- | -------------------- | ------------------ | ------------------ | --------------- |
| Manifest       | `package.json`                      | `deno.json(c)`    | `pyproject.toml`                         | `Cargo.toml`         | `go.mod`           | `Gemfile`          | `composer.json` |
| PM declaration | `packageManager`, `devEngines`      | n/a               | `[tool.uv]`, `[tool.poetry]`             | n/a                  | n/a                | n/a                | n/a             |
| Lockfile       | four, one per PM                    | `deno.lock`       | `uv.lock`, `poetry.lock`, `Pipfile.lock` | `Cargo.lock`         | `go.sum`           | `Gemfile.lock`     | `composer.lock` |
| Workspace      | `workspaces`, `pnpm-workspace.yaml` | `workspace`       | `[tool.uv.workspace]`                    | `[workspace]`        | `go.work`          | n/a                | n/a             |
| Frozen install | flag, or `npm ci`                   | `--frozen`        | `--frozen`, `--no-update`, `--deploy`    | `--locked`           | n/a                | `--frozen`         | n/a             |
| Script policy  | flag or env, PM specific            | `--allow-scripts` | n/a                                      | n/a                  | n/a                | n/a                | `--no-scripts`  |
| Exec primitive | `npx`, `bun x`, `pnpm exec`         | `deno x`          | `uvx`, `poetry run`                      | `cargo run --bin`    | `go run mod@ver`   | `bundle exec`      | `composer exec` |
| Exec reach     | network                             | network           | network for `uvx`                        | local                | network for `@ver` | local              | local           |
| Run file       | `node`, `bun`, `tsx`                | `deno run`        | `python`                                 | n/a                  | `go run file.go`   | `ruby`             | `php`           |
| Built-in test  | `node --test` with discovery        | `deno test`       | detect: pytest, nose2, …                 | `cargo test`         | `go test ./...`    | `rake test`        | n/a             |
| Task source    | `scripts`                           | `tasks`           | `[project.scripts]`                      | `[alias]`, `[[bin]]` | `cmd/<name>`       | n/a                | `scripts`       |
| Bin dirs       | `node_modules/.bin`                 | n/a               | `.venv/bin`, `Scripts/`                  | `target/…`           | `$GOBIN`           | `bin/` via bundler | `vendor/bin`    |
| Version scheme | npm semver                          | npm semver        | PEP 440                                  | cargo semver         | go module          | rubygems           | composer        |
| Runtime split  | node, bun, deno                     | itself            | interpreter per venv                     | n/a                  | n/a                | n/a                | n/a             |
| Quiet ladder   | `--silent`, `--loglevel`            | `-q`              | `-q`, `-qq`                              | `-q`                 | n/a                | `--quiet`          | `-q`, `-qq`     |
| Windows shim   | `.cmd`                              | `.exe`            | `.exe` in `Scripts/`                     | `.exe`               | `.exe`             | `.bat`             | `.bat`          |

Tool managers add one more axis each: mise, volta, asdf and proto declare
tools, expose bin dirs, and may or may not be activated in the shell that
ran runner. That is the `BinsCap::Ask` case plus `EnvVar` signals.

## 7. Core services

Provided once, used by every provider through the types above. A provider
never reimplements any of these.

- **Tree walk and scope.** Ancestor search, workspace member expansion,
  canonicalised containment.
- **Probe.** `PATH` and `PATHEXT` search with memoisation, plus extra dirs
  from `BinsCap` for resolution as well as spawning.
- **Template rendering.** Argv from `Template` plus policy.
- **Env layering.** Project, tool, task, in that order, filtered by trust.
- **Verbosity clamping.** Requested level to the strongest template the
  provider declares, recorded as a `Clamp`.
- **Reach gate.** The prompt and the `RUNNER_REACH` policy.
- **Labels, completion, schema, config validation.** All from the registry
  and the declaration table.
- **Rendering.** The arrow line, `why`, `doctor`, `--explain`, JSON, all
  from `Plan` and `Project`.
- **Warnings.** One sink, one ordering, one place that decides what a quiet
  preset hides.
- **Config init.** `runner config init` writes the schema pragma and every
  default the schema declares, live. The schema is the documentation.

## 8. Commands as stages

```rust
fn run(args) -> ExitStatus {
    let tree = Tree::open(args.dir)?;
    let policy = Policy::from(&args, &config::load(&tree)?);
    let evidence = observe(&tree);
    let project = resolve(&tree, evidence, &policy);
    let plan = plan(&project, &Op::from(&args), &policy)?;
    if args.explain { return render::explain(&plan); }
    render::arrow(&plan);
    execute(plan)
}
```

Every other subcommand is this function with a different stopping point.
`doctor` runs `Op::Health` for every present provider with a `HealthCap`
and appends the results to the project render.

## 9. Repository layout

The current tree is one crate with modules named after the tools they wrap.
Nothing in it stops `cmd/` from naming a tool, and nothing stops a tool
module from reading policy. Article 1 is enforceable by the compiler when
the crate boundary is the boundary.

```text
Cargo.toml                    workspace
crates/
  core/                       runner-core     types, pipeline, services, declaration table
  schemes/                    runner-schemes  version grammars, one module per ecosystem
  providers/                  runner-providers one file per provider, the registry
  cli/                        runner-run      the `runner` and `run` binaries, rendering, lsp, schema output
tests/                        integration tests over the binaries
fixtures/                     test projects, one directory per scenario
schemas/                      generated JSON schemas, committed
docs/
  architecture.md
  host-quiet-support-matrix.md
  notes/                      design notes and issue write-ups now at the root
packaging/
  npm/  aur/  man/  action/
action.yml  install.sh  install.ps1   fixed by GitHub and by documented URLs
site/
```

Dependency direction is the design:

```text
schemes  <-  core  <-  providers  <-  cli
```

`core` has no dependency on `providers`, so it cannot name a tool. `providers`
depends on `core` for the types and nothing else, so a provider cannot read
policy. `cli` sees everything and owns nothing but presentation and
argument parsing.

Inside each crate, one file per noun from section 4:

```text
crates/core/src/
  lib.rs
  tree.rs        scope.rs       signal.rs      evidence.rs
  provider.rs    capability.rs  template.rs    registry.rs
  op.rs          policy.rs      declare.rs     plan.rs
  observe.rs     resolve.rs     cascade.rs     execute.rs
  probe.rs       env.rs         verbosity.rs   reach.rs
  task.rs        health.rs      warning.rs     scheme.rs

crates/providers/src/
  lib.rs         registry.rs
  node/          npm.rs pnpm.rs yarn.rs bun.rs manifest.rs workspace.rs
  deno.rs        cargo.rs       go.rs
  python/        uv.rs poetry.rs pipenv.rs pyproject.rs venv.rs
  bundler.rs     composer.rs
  runners/       turbo.rs nx.rs make.rs just.rs task.rs bacon.rs
  managers/      mise.rs volta.rs

crates/cli/src/
  main.rs        bin/run.rs     args.rs
  commands/      run.rs install.rs clean.rs list.rs info.rs why.rs doctor.rs config.rs schema.rs completions.rs man.rs lsp/
  render/        arrow.rs explain.rs json.rs doctor.rs list.rs
  config/        load.rs  (declaration table lives in core, clap glue here)
```

`registry.rs` in `core` holds the `Provider` type and lookup by id, label
or alias. `registry.rs` in `providers` holds the static array. The split is
what lets `core` be tested with a fake registry.

Features stay on the `cli` crate: `lsp`, `schema`, `run`. `core` and
`providers` build without any.

Files that move without changing: `tests/` stays where Cargo expects it and
runs against the `cli` crate. `tests/fixtures/` becomes `fixtures/` so the
docker tests and the unit tests share one path. `og-plan.md`,
`proposal.md`, `TODO.md` and the issue write-ups move to `docs/notes/`. The
packaging files move together so a release touches one directory.

## 10. Migration

The current tests are the oracle for what must keep working, after an audit
that sorts each test into spec, pinned accident, or vacuous. Pinned
accidents are rewritten to the intended behaviour before the core exists,
and the resulting failures are the list of behaviour changes the rewrite
makes on purpose.

Steps, each a pull request with the suite green at the end:

1. The workspace split from section 9 with no behaviour change: the current
   crate becomes `crates/cli`, root files move, `fixtures/` and
   `docs/notes/` appear. Empty `core`, `schemes` and `providers` crates.
2. `core` gets the types in section 4. `providers` gets a registry that
   wraps the existing `tool::*` functions. Drift test: every current enum
   variant has a registry entry with matching label.
3. `install` on the core. It has the most branch sites today.
4. `run` on the core: task dispatch, exec, run file, test, the cascade
   table, the reach gate.
5. One resolver per ecosystem, the Python one deleted.
6. Labels, schema, completion and config validation from the registry. The
   per-setting parse functions deleted.
7. `observe` replaces `detect.rs`. `why` and `doctor` render from evidence.
8. Delete every `tool::*` free function the registry no longer calls. At
   this point `cli` contains no tool name.

## 11. Open questions

- `ProviderId` as one enum, or keep three enums and index all into the
  registry. One enum is simpler and matches the kinds-as-set model.
- Provider data as Rust statics, or TOML embedded at build time. Statics
  type-check the templates. TOML would let a user add a provider without a
  build.
- Polyglot `run test`: first present ecosystem by weight, every ecosystem in
  sequence, or refuse as `Ambiguous` and ask for `test:cargo`.
- Whether `Scope` needs a third variant for a nested workspace, a member
  that is itself a workspace root.
- Which schemes ship on day one. Ruby and Composer can start as `Unknown`.
- Whether `providers` is one crate or one crate per ecosystem. One crate
  keeps the registry a single array. Per ecosystem lets a build drop Ruby
  and PHP for a smaller binary.
- Publishing: `runner-run` stays the crates.io name for the binary crate.
  Whether `runner-core` is published at all, or stays a path dependency.
