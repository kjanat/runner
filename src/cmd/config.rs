//! `runner config` — scaffold, inspect, and validate the project-level
//! `runner.toml`. These actions operate on the file directly and must run
//! *before* the resolver's own `config::load` (which aborts on a malformed
//! file); `config validate` exists precisely to report that condition.

use std::fs;
use std::path::Path;

use anyhow::{Context as _, Result};
use colored::Colorize;

use crate::cli::ConfigAction;
use crate::config::{self, CONFIG_FILENAME, RunnerConfig};

/// Dispatch a `runner config <action>` subcommand. Returns the process exit
/// code: `0` on success, `2` when a file already exists (`init` without
/// `--force`) or fails validation.
pub(crate) fn config(dir: &Path, action: ConfigAction) -> Result<i32> {
    match action {
        ConfigAction::Init { force } => init(dir, force),
        ConfigAction::Show { json } => show(dir, json),
        ConfigAction::Validate => Ok(validate(dir)),
        ConfigAction::Path => Ok(path(dir)),
    }
}

/// `runner config init` — write the commented starter template to
/// `<dir>/runner.toml`. Refuses to clobber an existing file unless `force`.
fn init(dir: &Path, force: bool) -> Result<i32> {
    let target = dir.join(CONFIG_FILENAME);
    if target.exists() && !force {
        eprintln!(
            "{} {} already exists; pass {} to overwrite",
            "error:".red().bold(),
            target.display(),
            "--force".cyan(),
        );
        return Ok(2);
    }
    // config::INIT_TEMPLATE carries its own repo-relative `#:schema` pragma
    // (for editing this repo's copy) — swap it for the real published URL
    // rather than stacking a second pragma line in the user's project.
    let body = strip_leading_schema_pragma(config::INIT_TEMPLATE);
    let contents = format!("#:schema {}\n\n{body}", crate::schema::config_schema_url());
    fs::write(&target, contents)
        .with_context(|| format!("failed to write {}", target.display()))?;
    println!("{} {}", "wrote".green().bold(), target.display());
    Ok(0)
}

/// Strips a leading `#:schema ...` pragma line (plus one following blank
/// line) from `template`, or returns it unchanged if it doesn't start with
/// one. Not tied to the exact pragma text, so it keeps working if the
/// repo-relative path in `schemas/runner.init.toml` ever changes.
fn strip_leading_schema_pragma(template: &str) -> &str {
    let Some(rest) = template.strip_prefix("#:schema ") else {
        return template;
    };
    let Some((_, after_line)) = rest.split_once('\n') else {
        return template;
    };
    after_line.strip_prefix('\n').unwrap_or(after_line)
}

/// `runner config show` — render the effective config (file values merged
/// with built-in defaults) as TOML, or JSON with `--json`. Propagates parse
/// errors; use `validate` for a non-fatal diagnostic.
fn show(dir: &Path, json: bool) -> Result<i32> {
    let target = dir.join(CONFIG_FILENAME);
    let loaded = config::load(dir)?;
    let cfg = loaded
        .as_ref()
        .map_or_else(RunnerConfig::default, |l| l.config.clone());

    if json {
        let report = serde_json::json!({
            "path": target.display().to_string(),
            "exists": loaded.is_some(),
            "config": cfg,
        });
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        if loaded.is_some() {
            println!("{} {}", "config:".bold(), target.display());
        } else {
            println!(
                "{} {} {}",
                "config:".bold(),
                target.display(),
                "(not found — built-in defaults)".dimmed(),
            );
        }
        print!("{}", toml::to_string_pretty(&cfg)?);
    }
    Ok(0)
}

/// `runner config validate` — parse the file and run the same field and
/// failure-policy checks a live dispatch applies. Returns the exit code:
/// `0` when valid (or absent), `2` on any parse or policy error.
fn validate(dir: &Path) -> i32 {
    let target = dir.join(CONFIG_FILENAME);
    let loaded = match config::load(dir) {
        Ok(Some(loaded)) => loaded,
        Ok(None) => {
            println!(
                "{} no {} found (built-in defaults apply)",
                "ok:".green().bold(),
                CONFIG_FILENAME,
            );
            return 0;
        }
        Err(e) => {
            eprintln!("{} {:#}", "invalid:".red().bold(), e);
            return 2;
        }
    };

    // Unknown sections/fields are tolerated (forward compat) but worth
    // flagging here — a typo, or a key from a newer runner. They don't make
    // the file invalid, so they warn without changing the exit code.
    for warning in &loaded.warnings {
        eprintln!("{} {warning}", "warn:".yellow().bold());
    }

    if let Err(e) = crate::resolver::validate_config(&loaded) {
        eprintln!("{} {:#}", "invalid:".red().bold(), e);
        return 2;
    }
    println!("{} {} is valid", "ok:".green().bold(), target.display());
    0
}

/// `runner config path` — print the resolved `runner.toml` path (whether or
/// not it exists), one line, for scripting. Always succeeds.
fn path(dir: &Path) -> i32 {
    println!("{}", dir.join(CONFIG_FILENAME).display());
    0
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::config;
    use crate::cli::ConfigAction;
    use crate::config::CONFIG_FILENAME;
    use crate::tool::test_support::TempDir;

    #[test]
    fn init_writes_template_and_reparses() {
        let dir = TempDir::new("config-init");
        let code =
            config(dir.path(), ConfigAction::Init { force: false }).expect("init should succeed");
        assert_eq!(code, 0);

        let written = fs::read_to_string(dir.path().join(CONFIG_FILENAME))
            .expect("template should be written");
        // Line 1 is the `#:schema` directive so editors (tombi/taplo) wire up
        // autocompletion without any extra setup in the user's project.
        let first = written.lines().next().expect("template has a first line");
        assert!(
            first.starts_with("#:schema https://")
                && first.ends_with("schemas/runner.toml.schema.json"),
            "line 1 must be the schema directive, got: {first:?}"
        );
        // INIT_TEMPLATE carries its own repo-relative `#:schema` pragma (for
        // editing schemas/runner.init.toml in this repo); it must be swapped
        // for the absolute one above, not stacked alongside it.
        assert_eq!(
            written.matches("#:schema").count(),
            1,
            "exactly one #:schema directive, got:\n{written}"
        );
        // The scaffold must itself be valid (the directive is a comment, and
        // everything else is commented out → an empty, all-defaults config).
        assert_eq!(
            config(dir.path(), ConfigAction::Validate).expect("validate runs"),
            0,
            "scaffolded template must validate:\n{written}"
        );
    }

    #[test]
    fn strip_leading_schema_pragma_removes_pragma_and_blank_line() {
        let template = "#:schema ./runner.toml.schema.json\n\n# runner.toml\n[pm]\n";
        assert_eq!(
            super::strip_leading_schema_pragma(template),
            "# runner.toml\n[pm]\n"
        );
    }

    #[test]
    fn strip_leading_schema_pragma_leaves_non_pragma_content_untouched() {
        let template = "# runner.toml\n[pm]\n";
        assert_eq!(super::strip_leading_schema_pragma(template), template);
    }

    #[test]
    fn init_refuses_existing_without_force() {
        let dir = TempDir::new("config-init-existing");
        fs::write(dir.path().join(CONFIG_FILENAME), "[pm]\nnode = \"npm\"\n").expect("seed config");

        let code = config(dir.path(), ConfigAction::Init { force: false })
            .expect("init returns a code, not an error");
        assert_eq!(code, 2, "must refuse to clobber without --force");

        // Original content is untouched.
        let kept = fs::read_to_string(dir.path().join(CONFIG_FILENAME)).expect("read back");
        assert!(kept.contains("node = \"npm\""), "existing file preserved");
    }

    #[test]
    fn init_force_overwrites() {
        let dir = TempDir::new("config-init-force");
        fs::write(dir.path().join(CONFIG_FILENAME), "[pm]\nnode = \"npm\"\n").expect("seed config");

        let code =
            config(dir.path(), ConfigAction::Init { force: true }).expect("forced init succeeds");
        assert_eq!(code, 0);

        let written = fs::read_to_string(dir.path().join(CONFIG_FILENAME)).expect("read back");
        assert!(written.contains("# runner.toml"), "template replaced file");
    }

    #[test]
    fn validate_rejects_malformed_toml() {
        let dir = TempDir::new("config-validate-bad");
        fs::write(dir.path().join(CONFIG_FILENAME), "[pm]\nnode = \n").expect("seed broken config");

        let code = config(dir.path(), ConfigAction::Validate).expect("returns a code");
        assert_eq!(code, 2, "malformed TOML must fail validation");
    }

    #[test]
    fn validate_rejects_cross_ecosystem_pm() {
        let dir = TempDir::new("config-validate-pm");
        fs::write(dir.path().join(CONFIG_FILENAME), "[pm]\nnode = \"cargo\"\n")
            .expect("seed config");

        let code = config(dir.path(), ConfigAction::Validate).expect("returns a code");
        assert_eq!(code, 2, "cargo is not a node PM");
    }

    #[test]
    fn validate_rejects_both_chain_toggles() {
        // The combination the type still represents but the resolver
        // rejects: with no env var to neutralize a side, both-true is a
        // conflict.
        let dir = TempDir::new("config-validate-chain");
        fs::write(
            dir.path().join(CONFIG_FILENAME),
            "[chain]\nkeep_going = true\nkill_on_fail = true\n",
        )
        .expect("seed config");

        let code = config(dir.path(), ConfigAction::Validate).expect("returns a code");
        assert_eq!(
            code, 2,
            "keep_going + kill_on_fail both true must fail validation"
        );
    }

    #[test]
    fn validate_passes_clean_config() {
        let dir = TempDir::new("config-validate-ok");
        fs::write(
            dir.path().join(CONFIG_FILENAME),
            "[pm]\nnode = \"pnpm\"\n[chain]\nkeep_going = true\n",
        )
        .expect("seed config");

        let code = config(dir.path(), ConfigAction::Validate).expect("returns a code");
        assert_eq!(code, 0);
    }

    #[test]
    fn validate_absent_file_is_ok() {
        let dir = TempDir::new("config-validate-absent");
        let code = config(dir.path(), ConfigAction::Validate).expect("returns a code");
        assert_eq!(code, 0, "no file means defaults, which are valid");
    }
}
