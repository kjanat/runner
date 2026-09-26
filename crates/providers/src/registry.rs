//! The static provider table.

use runner_core::{Provider, Registry};

/// Every provider, in [`runner_core::ProviderId::ALL`] order.
pub static PROVIDERS: &[Provider] = &[
    crate::node::npm::PROVIDER,
    crate::node::yarn::PROVIDER,
    crate::node::pnpm::PROVIDER,
    crate::node::bun::PROVIDER,
    crate::deno::PROVIDER,
    crate::cargo::PROVIDER,
    crate::go::PROVIDER,
    crate::python::uv::PROVIDER,
    crate::python::poetry::PROVIDER,
    crate::python::pipenv::PROVIDER,
    crate::bundler::PROVIDER,
    crate::composer::PROVIDER,
    crate::runners::turbo::PROVIDER,
    crate::runners::nx::PROVIDER,
    crate::runners::make::PROVIDER,
    crate::runners::just::PROVIDER,
    crate::runners::task::PROVIDER,
    crate::managers::mise::PROVIDER,
    crate::runners::bacon::PROVIDER,
    crate::managers::volta::PROVIDER,
    crate::node::runtime::PROVIDER,
    crate::python::runtime::PROVIDER,
    crate::node::package_json::PROVIDER,
    crate::python::pyproject::PROVIDER,
    crate::powershell::PROVIDER,
];

/// Lookup over [`PROVIDERS`].
pub static REGISTRY: Registry = Registry(PROVIDERS);

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use runner_core::{Kind, NameShape, Piece, ProviderId, Reach, Request, ScriptRequest, Signal};

    use super::{PROVIDERS, REGISTRY};

    #[test]
    fn every_id_has_one_entry_in_id_order() {
        let ids: Vec<ProviderId> = PROVIDERS.iter().map(|provider| provider.id).collect();
        assert_eq!(ids, ProviderId::ALL);
    }

    #[test]
    fn labels_and_aliases_are_distinct() {
        let mut seen = HashSet::new();
        for provider in PROVIDERS {
            for spelling in std::iter::once(&provider.label).chain(provider.aliases) {
                assert!(seen.insert(*spelling), "{spelling} is declared twice");
            }
        }
    }

    #[test]
    fn every_label_and_alias_resolves_to_its_provider() {
        for provider in PROVIDERS {
            for spelling in std::iter::once(&provider.label).chain(provider.aliases) {
                assert_eq!(
                    REGISTRY.by_label(spelling).map(|found| found.id),
                    Some(provider.id)
                );
            }
        }
        assert!(REGISTRY.by_label("nothing").is_none());
    }

    #[test]
    fn a_provider_that_runs_anything_names_its_program() {
        for provider in PROVIDERS {
            let runs = provider.caps.install.is_some()
                || provider.caps.run_task.is_some()
                || provider.caps.exec.is_some()
                || provider.caps.run_file.is_some()
                || provider.caps.test.is_some();
            assert!(
                !runs || provider.program.is_some(),
                "{} runs commands without a program",
                provider.label
            );
        }
    }

    #[test]
    fn every_program_is_also_probed() {
        for provider in PROVIDERS {
            let Some(program) = provider.program else {
                continue;
            };
            assert!(
                provider
                    .signals
                    .iter()
                    .any(|signal| matches!(signal, Signal::Probe(name) if *name == program)),
                "{} does not probe for {program}",
                provider.label
            );
        }
    }

    #[test]
    fn a_task_source_dispatches_a_source_someone_declares() {
        for provider in PROVIDERS {
            let Some(run_task) = provider.caps.run_task else {
                continue;
            };
            for source in run_task.sources {
                assert!(
                    REGISTRY.by_id(*source).kind.contains(Kind::TASK_SOURCE),
                    "{} dispatches {source:?}, which declares no tasks",
                    provider.label
                );
            }
        }
    }

    #[test]
    fn quiet_pieces_only_appear_where_a_ladder_exists() {
        for provider in PROVIDERS {
            let mentions_quiet = [
                provider.caps.run_task.map(|cap| cap.argv),
                provider.caps.install.map(|cap| cap.argv),
            ]
            .into_iter()
            .flatten()
            .any(|template| template.0.contains(&Piece::Quiet));
            let has_ladder = provider.caps.quiet.strongest() > 0;
            assert_eq!(
                mentions_quiet, has_ladder,
                "{}: quiet piece and quiet ladder disagree",
                provider.label
            );
        }
    }

    fn words(rendered: &runner_core::Rendered) -> Vec<String> {
        rendered
            .args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn npm_frozen_install_renders_ci() {
        let install = REGISTRY.by_id(ProviderId::Npm).caps.install.unwrap();
        let rendered = install.argv.render(&Request {
            frozen: Some(install.frozen),
            scripts: Some((install.scripts, ScriptRequest::Deny)),
            ..Request::default()
        });
        assert_eq!(words(&rendered), ["ci", "--ignore-scripts"]);
    }

    #[test]
    fn quiet_sits_where_each_host_wants_it() {
        let args = ["--flag".to_owned()];
        let render = |id: ProviderId| {
            let provider = REGISTRY.by_id(id);
            let run = provider.caps.run_task.unwrap();
            words(&run.argv.render(&Request {
                task: Some("build"),
                args: &args,
                quiet: provider.caps.quiet.levels[1],
                ..Request::default()
            }))
        };
        assert_eq!(
            render(ProviderId::Npm),
            ["--silent", "run", "build", "--", "--flag"]
        );
        assert_eq!(
            render(ProviderId::Bun),
            ["run", "--silent", "build", "--flag"]
        );
        assert_eq!(render(ProviderId::Deno), ["task", "-q", "build", "--flag"]);
        assert_eq!(
            render(ProviderId::Mise),
            ["--quiet", "run", "build", "--", "--flag"]
        );
        assert_eq!(render(ProviderId::Cargo), ["-q", "build", "--flag"]);
        assert_eq!(render(ProviderId::Just), ["build", "--flag"]);
    }

    #[test]
    fn every_ladder_flag_and_root_invocation_matches_the_host() {
        let args = ["--flag".to_owned()];
        let render = |id: ProviderId, task: Option<&str>, quiet: bool| {
            let provider = REGISTRY.by_id(id);
            let run = provider.caps.run_task.unwrap();
            words(&run.argv.render(&Request {
                task,
                args: &args,
                quiet: quiet.then(|| provider.caps.quiet.at(1)).flatten(),
                stream: quiet.then_some(provider.caps.quiet.stream).flatten(),
                ..Request::default()
            }))
        };
        assert_eq!(
            render(ProviderId::Pnpm, Some("build"), true),
            ["--silent", "--use-stderr", "run", "build", "--", "--flag"]
        );
        let classic = REGISTRY
            .by_id(ProviderId::Yarn)
            .caps
            .variants
            .iter()
            .find_map(|(name, caps)| (*name == "classic").then_some(caps))
            .unwrap();
        assert_eq!(
            words(&classic.run_task.unwrap().argv.render(&Request {
                task: Some("build"),
                args: &args,
                quiet: classic.quiet.at(1),
                ..Request::default()
            })),
            ["--silent", "build", "--flag"]
        );
        assert_eq!(
            render(ProviderId::Yarn, Some("build"), true),
            ["run", "build", "--flag"]
        );
        assert_eq!(
            render(ProviderId::Make, Some("build"), true),
            ["-s", "build", "--flag"]
        );
        assert_eq!(render(ProviderId::Make, None, true), ["-s", "--flag"]);
        assert_eq!(
            render(ProviderId::Task, Some("build"), true),
            ["-s", "build", "--flag"]
        );
        assert_eq!(render(ProviderId::Just, None, true), ["--flag"]);
        assert_eq!(render(ProviderId::Bacon, None, false), ["--", "--flag"]);
        assert_eq!(
            render(ProviderId::Bacon, Some("check"), false),
            ["check", "--", "--flag"]
        );
        assert_eq!(
            render(ProviderId::Turbo, Some("build"), true),
            ["run", "build", "--", "--flag"]
        );
        assert_eq!(
            render(ProviderId::Uv, Some("cli"), true),
            ["--quiet", "run", "cli", "--flag"]
        );
        assert_eq!(
            render(ProviderId::Poetry, Some("cli"), true),
            ["--quiet", "run", "cli", "--flag"]
        );
        assert_eq!(
            render(ProviderId::Pipenv, Some("cli"), true),
            ["--quiet", "run", "cli", "--flag"]
        );
        assert_eq!(
            render(ProviderId::Go, Some("./cmd/x"), true),
            ["run", "./cmd/x", "--flag"]
        );
        assert_eq!(
            render(ProviderId::Node, Some("build"), true),
            ["--run", "build", "--", "--flag"]
        );
    }

    #[test]
    fn bun_as_the_chosen_runtime_puts_its_flag_before_the_subcommand() {
        let bun = REGISTRY.by_id(ProviderId::Bun);
        let runtime = bun.caps.as_runtime.unwrap();
        let args = ["--watch".to_owned()];
        let run = words(&runtime.run_task.unwrap().render(&Request {
            task: Some("build"),
            args: &args,
            quiet: bun.caps.quiet.at(1),
            ..Request::default()
        }));
        assert_eq!(run, ["--bun", "run", "--silent", "build", "--watch"]);
        let exec = words(&runtime.exec.unwrap().render(&Request {
            name: Some("eslint"),
            args: &args,
            ..Request::default()
        }));
        assert_eq!(exec, ["x", "--bun", "eslint", "--watch"]);
    }

    #[test]
    fn no_provider_grants_deno_permissions_or_invokes_a_shell() {
        for provider in PROVIDERS {
            for template in [
                provider.caps.install.map(|cap| cap.argv),
                provider.caps.run_task.map(|cap| cap.argv),
                provider.caps.exec.map(|cap| cap.argv),
                provider.caps.run_file.map(|cap| cap.argv),
                provider.caps.test.map(|cap| cap.argv),
            ]
            .into_iter()
            .flatten()
            {
                for piece in template.0 {
                    if let Piece::Lit(word) = piece {
                        assert!(
                            !word.starts_with("--allow-") && *word != "-A",
                            "{} grants a deno permission: {word}",
                            provider.label
                        );
                        assert!(
                            !matches!(*word, "-c" | "/c" | "-Command"),
                            "{} hands a command line to a shell: {word}",
                            provider.label
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn pnpm_diverts_its_own_output_to_stderr() {
        let pnpm = REGISTRY.by_id(ProviderId::Pnpm);
        assert_eq!(
            pnpm.caps.quiet.stream,
            Some(runner_core::t!["--use-stderr"])
        );
    }

    /// Render a capability's template the way a plan does, program first.
    fn rendered(
        program: Option<&str>,
        template: runner_core::Template,
        request: &Request<'_>,
    ) -> Vec<String> {
        let mut words: Vec<String> = program.map(ToOwned::to_owned).into_iter().collect();
        words.extend(
            template
                .render(request)
                .args
                .iter()
                .map(|arg| arg.to_string_lossy().into_owned()),
        );
        words
    }

    #[test]
    fn every_exec_primitive_matches_the_ecosystem_table() {
        let args = [String::from("--fix")];
        let request = Request {
            name: Some("eslint"),
            args: &args,
            ..Request::default()
        };
        let bare_or_versioned = NameShape::BARE.union(NameShape::VERSIONED);
        let expected: &[(ProviderId, &[&str], NameShape, Reach)] = &[
            (
                ProviderId::Npm,
                &["npx", "eslint", "--fix"],
                bare_or_versioned,
                Reach::Network,
            ),
            (
                ProviderId::Yarn,
                &["yarn", "run", "eslint", "--fix"],
                NameShape::BARE,
                Reach::Local,
            ),
            (
                ProviderId::Pnpm,
                &["pnpm", "exec", "eslint", "--fix"],
                NameShape::BARE,
                Reach::Local,
            ),
            (
                ProviderId::Bun,
                &["bun", "x", "eslint", "--fix"],
                bare_or_versioned,
                Reach::Network,
            ),
            (
                ProviderId::Deno,
                &["deno", "x", "eslint", "--fix"],
                bare_or_versioned.union(NameShape::REGISTRY),
                Reach::Network,
            ),
            (
                ProviderId::Uv,
                &["uvx", "eslint", "--fix"],
                bare_or_versioned,
                Reach::Network,
            ),
            (
                ProviderId::Go,
                &["go", "run", "eslint", "--fix"],
                NameShape::PATH_LIKE.union(NameShape::VERSIONED),
                Reach::Network,
            ),
            (
                ProviderId::Mise,
                &["mise", "exec", "--", "eslint", "--fix"],
                NameShape::BARE,
                Reach::Network,
            ),
            (
                ProviderId::Node,
                &["npx", "eslint", "--fix"],
                bare_or_versioned,
                Reach::Network,
            ),
        ];
        for (id, argv, accepts, reach) in expected {
            let provider = REGISTRY.by_id(*id);
            let cap = provider
                .caps
                .exec
                .unwrap_or_else(|| panic!("{} has no exec primitive", provider.label));
            assert_eq!(
                rendered(cap.program.or(provider.program), cap.argv, &request),
                *argv,
                "{}",
                provider.label
            );
            assert_eq!(cap.accepts, *accepts, "{}", provider.label);
            assert_eq!(cap.reach, *reach, "{}", provider.label);
        }
        for id in [ProviderId::Cargo, ProviderId::Bundler, ProviderId::Composer] {
            assert!(
                REGISTRY.by_id(id).caps.exec.is_none(),
                "{} has a local exec primitive, not a fetching one",
                REGISTRY.by_id(id).label
            );
        }
    }

    #[test]
    fn a_path_like_name_never_reaches_a_bare_only_primitive() {
        for id in [
            ProviderId::Npm,
            ProviderId::Bun,
            ProviderId::Deno,
            ProviderId::Uv,
        ] {
            let cap = REGISTRY.by_id(id).caps.exec.expect("an exec primitive");
            assert!(!cap.accepts.contains(NameShape::PATH_LIKE));
        }
        let go = REGISTRY.by_id(ProviderId::Go).caps.exec.expect("go run");
        assert!(!go.accepts.contains(NameShape::BARE));
    }

    #[test]
    fn every_built_in_test_runner_matches_the_ecosystem_table() {
        let expected: &[(ProviderId, &[&str])] = &[
            (ProviderId::Npm, &["node", "--test"]),
            (ProviderId::Yarn, &["node", "--test"]),
            (ProviderId::Pnpm, &["node", "--test"]),
            (ProviderId::Node, &["node", "--test"]),
            (ProviderId::Bun, &["bun", "test"]),
            (ProviderId::Deno, &["deno", "test"]),
            (ProviderId::Cargo, &["cargo", "test"]),
            (ProviderId::Go, &["go", "test", "./..."]),
            (ProviderId::Bundler, &["rake", "test"]),
        ];
        for (id, argv) in expected {
            let provider = REGISTRY.by_id(*id);
            let cap = provider
                .caps
                .test
                .unwrap_or_else(|| panic!("{} has no test runner", provider.label));
            assert_eq!(
                rendered(
                    cap.program.or(provider.program),
                    cap.argv,
                    &Request::default()
                ),
                *argv,
                "{}",
                provider.label
            );
        }
        assert!(
            REGISTRY.by_id(ProviderId::Composer).caps.test.is_none(),
            "PHP has no built-in test runner"
        );
    }

    #[test]
    fn node_finds_its_own_test_files_and_the_others_find_their_own() {
        let node = REGISTRY
            .by_id(ProviderId::Node)
            .caps
            .test
            .expect("node --test");
        let runner_core::Discovery::Files(patterns) = node.discovery else {
            panic!("node --test is handed its files");
        };
        assert!(patterns.contains(&"test.ts"));
        assert!(patterns.contains(&"*.test.ts"));
        assert!(patterns.iter().all(|pattern| {
            std::path::Path::new(pattern)
                .extension()
                .is_some_and(|ext| {
                    !ext.eq_ignore_ascii_case("jsx") && !ext.eq_ignore_ascii_case("tsx")
                })
        }));
        for id in [
            ProviderId::Bun,
            ProviderId::Deno,
            ProviderId::Cargo,
            ProviderId::Go,
        ] {
            let cap = REGISTRY.by_id(id).caps.test.expect("a test runner");
            assert!(
                matches!(cap.discovery, runner_core::Discovery::Tool),
                "{} finds its own tests",
                REGISTRY.by_id(id).label
            );
        }
        for id in [ProviderId::Uv, ProviderId::Poetry, ProviderId::Pipenv] {
            let cap = REGISTRY.by_id(id).caps.test.expect("a test runner");
            assert!(
                matches!(cap.discovery, runner_core::Discovery::Detect(_)),
                "a Python test runner is itself a finding"
            );
        }
    }

    #[test]
    fn the_python_test_runner_is_detected_in_the_documented_order() {
        let dir = std::env::temp_dir().join(format!("runner-py-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let words = |template: runner_core::Template| {
            template
                .render(&Request::default())
                .args
                .iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        };
        let runner = |dir: &std::path::Path| {
            words(
                crate::python::test_runner(&[dir])
                    .unwrap()
                    .expect("unittest is the floor"),
            )
        };

        assert_eq!(runner(&dir), ["run", "python", "-m", "unittest"]);

        std::fs::write(dir.join("noxfile.py"), "").expect("noxfile");
        std::fs::write(dir.join("tox.ini"), "").expect("tox.ini");
        std::fs::write(dir.join("manage.py"), "").expect("manage.py");
        assert_eq!(runner(&dir), ["run", "python", "manage.py", "test"]);

        let bin = dir.join(".venv").join("bin");
        std::fs::create_dir_all(&bin).expect("venv bin");
        for (name, expected) in [
            ("ward", vec!["run", "ward"]),
            ("nose2", vec!["run", "nose2"]),
            ("pytest", vec!["run", "pytest"]),
        ] {
            std::fs::write(bin.join(name), "").expect("venv entry");
            assert_eq!(runner(&dir), expected, "{name}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
