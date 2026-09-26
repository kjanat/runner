//! Build [`ResolutionOverrides`] from the command line, the environment and
//! `runner.toml`.

use std::collections::BTreeMap;
use std::io::IsTerminal as _;

use anyhow::{Result, anyhow};

use super::types::{
    DownloadPolicy, EnvLayers, LockfilePolicy, Output, OverrideOrigin, ParentMarkers, PmOverride,
    ResolutionOverrides, RuntimeOverride, ScriptPolicy, SourceOverride, TaskChoice,
};
use crate::chain::FailurePolicy;
use crate::config::{Download, LoadedConfig, RunnerConfig, TaskOutput};
use crate::invocation::Origin;
use crate::tool::{OutputChoice, QuietLevel, RunnerOutput};
use crate::types::DetectionWarning;
use runner_core::ProviderId;

/// The values the command line and `RUNNER_*` variables supplied, each with
/// the layer that supplied it.
#[derive(Debug, Clone, Default)]
pub(crate) struct Invocation {
    pub pm: Option<(ProviderId, Origin)>,
    pub runtime: Option<(ProviderId, Origin)>,
    pub source: Option<(ProviderId, Origin)>,
    pub package: Option<String>,
    pub download: Option<(Download, Origin)>,
    pub on_fail: Option<(FailurePolicy, Origin)>,
    pub dry_run: bool,
    /// The `-q` preset each layer gave, strongest first.
    pub quiet: Vec<(u8, Origin)>,
    pub warnings: Option<(bool, Origin)>,
    pub frozen: Option<(bool, Origin)>,
    pub scripts: Option<(bool, Origin)>,
    pub tools: Option<(bool, Origin)>,
    /// The inherited `RUNNER_GROUP_ACTIVE` marker.
    pub group_active: bool,
}

/// Whether the invocation can put a question to the user.
fn interactive() -> bool {
    std::io::stdin().is_terminal()
        && std::io::stderr().is_terminal()
        && !actions_rs::env::is_github_actions()
        && !actions_rs::env::is_ci()
}

impl ResolutionOverrides {
    /// Resolve every setting.
    ///
    /// # Errors
    /// The first `runner.toml` value that names no provider of its kind.
    pub(crate) fn resolve(invocation: &Invocation, config: Option<&LoadedConfig>) -> Result<Self> {
        let (resolved, issues) = Self::resolve_lenient(invocation, config);
        match issues.into_iter().next() {
            Some(DetectionWarning::InvalidConfigValue { key, message, .. }) => {
                Err(anyhow!("runner.toml {key}: {message}"))
            }
            Some(other) => Err(anyhow!("{other}")),
            None => Ok(resolved),
        }
    }

    /// Resolve every setting, dropping each `runner.toml` value that names no
    /// provider of its kind and reporting it instead.
    pub(crate) fn resolve_lenient(
        invocation: &Invocation,
        config: Option<&LoadedConfig>,
    ) -> (Self, Vec<DetectionWarning>) {
        let mut issues = Vec::new();
        let file = config.map(|loaded| &loaded.config);
        let project_origin = || {
            config.map(|loaded| OverrideOrigin::ConfigFile {
                path: loaded.path.clone(),
            })
        };
        let project_runtime = file
            .and_then(|file| file.runtime.javascript.as_deref())
            .and_then(|raw| {
                label(
                    &mut issues,
                    ["runtime", "javascript"].into(),
                    raw,
                    crate::provider::parse_js_runtime,
                )
            });
        let tasks = file.map_or_else(Default::default, |file| task_choices(file, &mut issues));
        let quiet_level = invocation
            .quiet
            .first()
            .map_or(QuietLevel::Off, |(count, _)| QuietLevel::from_count(*count));
        let install = file.map(|file| &file.install);
        let frozen = invocation
            .frozen
            .map(|(value, _)| value)
            .or_else(|| install.and_then(|install| install.frozen))
            .unwrap_or(false);
        let scripts = invocation
            .scripts
            .map(|(value, _)| value)
            .or_else(|| install.and_then(|install| install.scripts));

        let resolved = Self {
            pm: invocation.pm.map(|(pm, origin)| PmOverride {
                pm,
                origin: OverrideOrigin::from(origin),
            }),
            package: invocation.package.clone(),
            source: invocation.source.map(|(source, origin)| SourceOverride {
                source,
                origin: OverrideOrigin::from(origin),
            }),
            runtime: invocation
                .runtime
                .map(|(runtime, origin)| RuntimeOverride {
                    runtime,
                    origin: OverrideOrigin::from(origin),
                })
                .or_else(|| {
                    Some(RuntimeOverride {
                        runtime: project_runtime?,
                        origin: project_origin()?,
                    })
                }),
            quiet_level,
            output: Output {
                invocation: invocation_output(invocation, Origin::Cli)
                    .over(invocation_output(invocation, Origin::Env)),
                project: file.map_or_else(OutputChoice::default, project_output),
                buffer: file.and_then(|file| file.output.parallel.buffer),
            },
            dry_run: invocation.dry_run,
            failure_policy: invocation
                .on_fail
                .map(|(policy, _)| policy)
                .or_else(|| file.and_then(|file| file.chain.on_fail))
                .unwrap_or_default(),
            script_policy: scripts.map_or(ScriptPolicy::Default, ScriptPolicy::from_setting),
            lockfile: if frozen {
                LockfilePolicy::Frozen
            } else {
                LockfilePolicy::Update
            },
            install_tools: invocation
                .tools
                .map(|(value, _)| value)
                .or_else(|| install.and_then(|install| install.tools))
                .unwrap_or(true),
            download: download_policy(invocation, file),
            env: file.map_or_else(EnvLayers::default, env_layers),
            tasks,
            config: config.map(|loaded| loaded.path.clone()),
            parent: ParentMarkers {
                group_open: invocation.group_active,
                warned: false,
            },
        };
        (resolved, issues)
    }
}

/// `raw` parsed by `parse`, or `None` with the failure recorded against `key`.
fn label(
    issues: &mut Vec<DetectionWarning>,
    key: crate::config::KeyPath,
    raw: &str,
    parse: fn(&str) -> Result<ProviderId, String>,
) -> Option<ProviderId> {
    parse(raw)
        .map_err(|message| {
            issues.push(DetectionWarning::InvalidConfigValue {
                key,
                raw: raw.to_owned(),
                message,
            });
        })
        .ok()
}

/// Every `[tasks.<name>]` table's choices.
fn task_choices(
    file: &RunnerConfig,
    issues: &mut Vec<DetectionWarning>,
) -> BTreeMap<String, TaskChoice> {
    file.tasks
        .iter()
        .map(|(name, settings)| {
            let choice = TaskChoice {
                source: settings.source.as_deref().and_then(|raw| {
                    label(
                        issues,
                        ["tasks", name, "source"].into(),
                        raw,
                        crate::provider::parse_task_source,
                    )
                }),
                pm: settings.pm.as_deref().and_then(|raw| {
                    label(
                        issues,
                        ["tasks", name, "pm"].into(),
                        raw,
                        crate::provider::parse_package_manager,
                    )
                }),
                runtime: settings.runtime.javascript.as_deref().and_then(|raw| {
                    label(
                        issues,
                        ["tasks", name, "runtime", "javascript"].into(),
                        raw,
                        crate::provider::parse_js_runtime,
                    )
                }),
                output: task_output(&settings.output),
            };
            (name.clone(), choice)
        })
        .collect()
}

/// The download policy: the invocation's, else the file's, else ask on an
/// interactive terminal and allow elsewhere.
fn download_policy(invocation: &Invocation, file: Option<&RunnerConfig>) -> DownloadPolicy {
    match (invocation.download, file.and_then(|file| file.download)) {
        (Some((value, _)), _) | (None, Some(value)) => DownloadPolicy {
            value,
            explicit: true,
        },
        (None, None) => DownloadPolicy {
            value: if interactive() {
                Download::Ask
            } else {
                Download::Allow
            },
            explicit: false,
        },
    }
}

/// The `[output]` table's choices.
fn project_output(file: &RunnerConfig) -> OutputChoice {
    task_output(&file.output.task)
        .with_some(RunnerOutput::Warnings, file.output.warnings)
        .with_some(RunnerOutput::Errors, file.output.errors)
        .with_some(RunnerOutput::FatalErrors, file.output.errors)
        .with_some(RunnerOutput::Summary, file.output.summary)
}

/// The `[env]`, `[tools.<name>.env]` and `[tasks.<name>.env]` maps.
fn env_layers(file: &RunnerConfig) -> EnvLayers {
    fn named<'a>(
        entries: impl Iterator<Item = (&'a String, &'a BTreeMap<String, String>)>,
    ) -> BTreeMap<String, BTreeMap<String, String>> {
        entries
            .filter(|(_, env)| !env.is_empty())
            .map(|(name, env)| (name.clone(), env.clone()))
            .collect()
    }
    EnvLayers {
        project: file.env.clone(),
        tool: named(file.tools.iter().map(|(name, tool)| (name, &tool.env))),
        task: named(file.tasks.iter().map(|(name, task)| (name, &task.env))),
    }
}

/// The output one layer of the invocation sets: its `-q` preset, then its
/// individual flags over it.
fn invocation_output(invocation: &Invocation, layer: Origin) -> OutputChoice {
    let at = |value: Option<(bool, Origin)>| value.filter(|(_, origin)| *origin == layer);
    let preset = invocation
        .quiet
        .iter()
        .find(|(_, origin)| *origin == layer)
        .map_or_else(OutputChoice::default, |(count, _)| {
            OutputChoice::preset(QuietLevel::from_count(*count))
        });
    preset.with_some(
        RunnerOutput::Warnings,
        at(invocation.warnings).map(|(value, _)| value),
    )
}

fn task_output(output: &TaskOutput) -> OutputChoice {
    OutputChoice::default()
        .with_some(RunnerOutput::Progress, output.progress)
        .with_some(RunnerOutput::Groups, output.groups)
        .with_some(RunnerOutput::Timing, output.timing)
        .with_tool_quiet(output.tool.quiet)
        .with_streams(output.task.stdout, output.task.stderr)
}

/// Validate a loaded `runner.toml` the way a dispatch would read it.
///
/// # Errors
/// The first value that names no provider of its kind.
pub(crate) fn validate_config(loaded: &LoadedConfig) -> Result<()> {
    ResolutionOverrides::resolve(&Invocation::default(), Some(loaded)).map(drop)
}

/// Every value in a loaded `runner.toml` that names no provider of its kind.
pub(crate) fn config_issues(loaded: &LoadedConfig) -> Vec<DetectionWarning> {
    ResolutionOverrides::resolve_lenient(&Invocation::default(), Some(loaded)).1
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{Invocation, ResolutionOverrides};
    use crate::chain::FailurePolicy;
    use crate::config::{Download, LoadedConfig};
    use crate::invocation::Origin;
    use crate::resolver::{LockfilePolicy, OverrideOrigin, ScriptPolicy};
    use crate::tool::{HostDiagnostics, RunnerOutput, TaskStream};
    use crate::types::DetectionWarning;
    use runner_core::ProviderId;

    fn config(body: &str) -> LoadedConfig {
        LoadedConfig {
            path: PathBuf::from("/p/runner.toml"),
            config: toml::from_str(body).expect("parses"),
            warnings: Vec::new(),
        }
    }

    fn resolve(invocation: &Invocation, body: &str) -> ResolutionOverrides {
        ResolutionOverrides::resolve(invocation, Some(&config(body))).expect("resolves")
    }

    #[test]
    fn a_task_leaf_overrides_the_project_leaf_and_inherits_the_rest() {
        let resolved = resolve(
            &Invocation::default(),
            "[output]\ntiming = false\nprogress = false\n[output.task]\nstdout = true\nstderr = \
             false\n[tasks.build.output]\ntiming = true\n[tasks.build.output.task]\nstderr = \
             true\n",
        );
        let build = resolved.output_for(Some("build"));
        assert!(build.runner.shows(RunnerOutput::Timing));
        assert!(!build.runner.shows(RunnerOutput::Progress));
        assert_eq!(build.stderr, TaskStream::Inherit);
        let lint = resolved.output_for(Some("lint"));
        assert!(!lint.runner.shows(RunnerOutput::Timing));
        assert_eq!(lint.stderr, TaskStream::Discard);
    }

    #[test]
    fn the_command_line_beats_the_environment_beats_the_task_beats_the_project() {
        let body =
            "[runtime]\njavascript = \"node\"\n[tasks.build.runtime]\njavascript = \"bun\"\n";
        let resolved = resolve(&Invocation::default(), body);
        assert_eq!(
            resolved.runtime_for("build").map(|over| over.runtime),
            Some(ProviderId::Bun)
        );
        assert_eq!(
            resolved.runtime_for("lint").map(|over| over.runtime),
            Some(ProviderId::Node)
        );
        let cli = resolve(
            &Invocation {
                runtime: Some((ProviderId::Node, Origin::Cli)),
                ..Invocation::default()
            },
            body,
        );
        let chosen = cli.runtime_for("build").expect("runtime");
        assert_eq!(chosen.runtime, ProviderId::Node);
        assert_eq!(chosen.origin, OverrideOrigin::CliFlag);
    }

    #[test]
    fn an_explicit_false_is_a_value() {
        let resolved = resolve(
            &Invocation {
                scripts: Some((true, Origin::Cli)),
                frozen: Some((false, Origin::Env)),
                ..Invocation::default()
            },
            "[install]\nscripts = false\nfrozen = true\ntools = false\n",
        );
        assert_eq!(resolved.script_policy, ScriptPolicy::Allow);
        assert_eq!(resolved.lockfile, LockfilePolicy::Update);
        assert!(!resolved.install_tools);
        let unset = resolve(&Invocation::default(), "");
        assert_eq!(unset.script_policy, ScriptPolicy::Default);
        assert!(unset.install_tools);
    }

    #[test]
    fn a_quiet_preset_yields_to_an_explicit_flag_of_its_own_layer_only() {
        let resolved = resolve(
            &Invocation {
                quiet: vec![(2, Origin::Env)],
                warnings: Some((true, Origin::Env)),
                ..Invocation::default()
            },
            "[output]\nprogress = true\n",
        );
        assert!(resolved.shows_warnings());
        assert!(!resolved.shows_progress());
        assert_eq!(
            resolved.host_verbosity_for("build").diagnostics,
            HostDiagnostics::Quiet
        );
        let cli_preset = resolve(
            &Invocation {
                quiet: vec![(2, Origin::Cli)],
                warnings: Some((true, Origin::Env)),
                ..Invocation::default()
            },
            "",
        );
        assert!(!cli_preset.shows_warnings());
    }

    #[test]
    fn a_weaker_command_line_preset_keeps_what_only_the_environment_preset_sets() {
        let both = resolve(
            &Invocation {
                quiet: vec![(1, Origin::Cli), (2, Origin::Env)],
                ..Invocation::default()
            },
            "",
        );
        assert!(!both.shows_progress());
        assert!(!both.shows_warnings());
        assert_eq!(
            both.host_verbosity_for("build").diagnostics,
            HostDiagnostics::Quiet
        );
        let restored = resolve(
            &Invocation {
                quiet: vec![(1, Origin::Cli), (2, Origin::Env)],
                warnings: Some((true, Origin::Cli)),
                ..Invocation::default()
            },
            "",
        );
        assert!(restored.shows_warnings());
        assert!(!restored.shows_progress());
        assert_eq!(
            restored.host_verbosity_for("build").diagnostics,
            HostDiagnostics::Quiet
        );
    }

    #[test]
    fn failure_policy_comes_from_the_highest_layer() {
        let body = "[chain]\non_fail = \"kill\"\n";
        assert_eq!(
            resolve(&Invocation::default(), body).failure_policy,
            FailurePolicy::Kill
        );
        let over = resolve(
            &Invocation {
                on_fail: Some((FailurePolicy::Continue, Origin::Cli)),
                ..Invocation::default()
            },
            body,
        );
        assert_eq!(over.failure_policy, FailurePolicy::Continue);
        assert_eq!(
            resolve(&Invocation::default(), "").failure_policy,
            FailurePolicy::Wait
        );
    }

    #[test]
    fn an_explicit_download_setting_is_marked_explicit() {
        let resolved = resolve(&Invocation::default(), "download = \"ask\"\n");
        assert_eq!(resolved.download.value, Download::Ask);
        assert!(resolved.download.explicit);
        let unset = resolve(&Invocation::default(), "");
        assert!(!unset.download.explicit);
    }

    #[test]
    fn env_layers_keep_every_level() {
        let resolved = resolve(
            &Invocation::default(),
            "[env]\nA = \"1\"\n[tools.mise.env]\nB = \"2\"\n[tasks.build.env]\nA = \"3\"\n",
        );
        assert_eq!(resolved.env.project["A"], "1");
        assert_eq!(resolved.env.tool["mise"]["B"], "2");
        assert_eq!(resolved.env.task["build"]["A"], "3");
    }

    #[test]
    fn a_bad_provider_name_fails_strictly_and_is_dropped_leniently() {
        let loaded = config("[tasks.build]\npm = \"pnpmm\"\nsource = \"just\"\n");
        let error = ResolutionOverrides::resolve(&Invocation::default(), Some(&loaded))
            .expect_err("unknown package manager");
        assert!(format!("{error}").contains("tasks.build.pm"), "{error}");
        let (resolved, issues) =
            ResolutionOverrides::resolve_lenient(&Invocation::default(), Some(&loaded));
        assert!(matches!(
            issues.as_slice(),
            [DetectionWarning::InvalidConfigValue { key, .. }] if key.to_string() == "tasks.build.pm"
        ));
        assert_eq!(resolved.task("build").source, Some(ProviderId::Just));
        assert_eq!(resolved.task("build").pm, None);
    }

    #[test]
    fn qualified_task_keys_layer_over_the_bare_name() {
        let resolved = resolve(
            &Invocation::default(),
            "[tasks.build]\npm = \"npm\"\n[tasks.\"package.json:build\"]\nsource = \
             \"package.json\"\n",
        );
        let task = resolved.task("package.json:build");
        assert_eq!(task.pm, Some(ProviderId::Npm));
        assert_eq!(task.source, Some(ProviderId::PackageJson));
    }
}
