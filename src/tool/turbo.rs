//! Turborepo — monorepo build system.

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use anyhow::Context as _;
use serde::Deserialize;

/// Directories produced by Turborepo.
pub(crate) const CLEAN_DIRS: &[&str] = &[".turbo"];

/// Main Turborepo config filename.
pub(crate) const FILENAME: &str = "turbo.json";

/// Detected via `turbo.json`.
pub(crate) fn detect(dir: &Path) -> bool {
    dir.join(FILENAME).exists()
}

/// Parse task names from `turbo.json`.
///
/// Supports both v2 (`"tasks"`) and v1 (`"pipeline"`) schemas. Scoped
/// tasks like `"my-app#build"` are filtered out.
pub(crate) fn extract_tasks(dir: &Path) -> anyhow::Result<Vec<String>> {
    #[derive(Deserialize)]
    struct Partial {
        tasks: Option<HashMap<String, serde_json::Value>>,
        pipeline: Option<HashMap<String, serde_json::Value>>,
    }
    let path = dir.join(FILENAME);
    if !path.exists() {
        return Ok(vec![]);
    }
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let p = serde_json::from_str::<Partial>(&content)
        .with_context(|| format!("{} is not valid JSON", path.display()))?;
    let Some(tasks) = p.tasks.or(p.pipeline) else {
        return Ok(vec![]);
    };
    Ok(tasks
        .into_keys()
        .filter(|name| !name.contains('#'))
        .collect())
}

/// `turbo run <task> [-- args...]`
pub(crate) fn run_cmd(task: &str, args: &[String]) -> Command {
    let mut c = Command::new("turbo");
    c.arg("run").arg(task);
    if !args.is_empty() {
        c.arg("--").args(args);
    }
    c
}

/// Returns `true` if `command` is a thin invocation of `turbo` that targets
/// the same task name — i.e. `turbo run <name>` or the shorthand
/// `turbo <name>`, optionally followed by flag tokens (e.g. `--filter web`,
/// `--concurrency=4`).
///
/// The tail after the target name must consist solely of flag tokens
/// (`-x`, `--long`, `--key=value`) or values immediately following a
/// non-`=` flag. Any of these reject the match — they mean the script
/// does more than just dispatch to turbo:
/// - shell control operators: `&&`, `||`, `;`, `;;`, `;&`, `;;&`,
///   `|`, `|&`, `&`, `!`, `{`, `}`, `(`, `)`
/// - redirect operators: bare `>`/`<`/`>>`/`<<`/`<<<`, combined-fd
///   `&>`/`&>>`/`>&`, fd-prefixed `2>`, `1>`, composite `2>&1`,
///   `1>&2`, `2>/dev/null`, `&>file.log`
/// - any other bare positional that isn't consuming a flag's value
///
/// This is purely a textual heuristic on the script body. Indirect
/// invocations (`npx turbo run build`, `pnpm exec turbo run build`) are
/// intentionally not matched: a wrapper that goes through a package-manager
/// shim is a step removed from the canonical Turborepo pattern, and matching
/// it would risk false positives for unrelated `npx`/`pnpm exec` scripts.
///
/// Known limitations (deferred — rare in turbo dispatch scripts and
/// stricter detection would over-reject legitimate patterns):
/// - shell substitution (`$(...)`, backticks) following a flag is
///   silently accepted as a flag value;
/// - unquoted globs (`*`, `?`) following a flag are accepted because
///   `*` is legitimate in turbo filters like `@scope/*`;
/// - quoted multi-word arguments (`--filter "my app"`) are split
///   incorrectly by `split_whitespace` and reject via the positional
///   rule — false negative, safe direction (script stays visible).
pub(crate) fn is_self_passthrough(name: &str, command: &str) -> bool {
    let mut tokens = command.split_whitespace();
    if tokens.next() != Some("turbo") {
        return false;
    }
    let Some(second) = tokens.next() else {
        return false;
    };
    let target = if second == "run" {
        let Some(third) = tokens.next() else {
            return false;
        };
        third
    } else {
        second
    };
    if target != name {
        return false;
    }

    // Tail must be flags-only (with optional space-separated values).
    // Any bare positional, shell metachar, or redirect operator means
    // the script is doing extra work and is not a thin passthrough.
    //
    // Order matters: redirect detection runs BEFORE flag-value
    // consumption so that fd-style redirects like `2>&1` after a
    // value-expecting flag are rejected, not silently swallowed as
    // the flag's value.
    let mut expects_flag_value = false;
    for token in tokens {
        if is_shell_control_token(token) {
            return false;
        }
        if looks_like_redirect(token) {
            return false;
        }
        if token.starts_with('-') {
            expects_flag_value = !token.contains('=');
            continue;
        }
        if expects_flag_value {
            expects_flag_value = false;
            continue;
        }
        return false;
    }
    true
}

/// Detects standalone bash control operators that introduce extra
/// behavior beyond a thin turbo dispatch:
/// - list operators (`&&`, `||`)
/// - command separators (`;`, `;;`, `;&`, `;;&`)
/// - pipes (`|`, `|&`)
/// - background (`&`)
/// - pipeline negation (`!`)
/// - group/subshell delimiters (`{`, `}`, `(`, `)`)
fn is_shell_control_token(token: &str) -> bool {
    matches!(
        token,
        "&&" | "||" | ";" | ";;" | ";&" | ";;&" | "|" | "|&" | "&" | "!" | "{" | "}" | "(" | ")"
    )
}

/// Detects bash redirect operators in any of these forms:
/// - bare: `>`, `>>`, `<`, `<<`, `<<<`
/// - combined-fd: `&>`, `&>>`, `>&`
/// - fd-prefixed: `2>`, `1>`, `3<`, …
/// - composite: `2>&1`, `1>&2`, `2>/dev/null`, `&>file.log`, `>file`
///
/// The strategy: strip optional leading file-descriptor digits, then
/// optional leading `&` (for combined-fd forms), then check whether
/// what remains starts with `>` or `<`.
fn looks_like_redirect(token: &str) -> bool {
    let rest = token.trim_start_matches(|c: char| c.is_ascii_digit());
    let rest = rest.strip_prefix('&').unwrap_or(rest);
    rest.starts_with('>') || rest.starts_with('<')
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::extract_tasks;
    use crate::tool::test_support::TempDir;

    #[test]
    fn extract_tasks_returns_empty_when_turbo_json_is_missing() {
        let dir = TempDir::new("turbo-missing");

        assert!(
            extract_tasks(dir.path())
                .expect("missing turbo.json should be ok")
                .is_empty()
        );
    }

    #[test]
    fn extract_tasks_errors_on_malformed_json() {
        let dir = TempDir::new("turbo-malformed");
        fs::write(dir.path().join("turbo.json"), "{").expect("turbo.json should be written");

        assert!(extract_tasks(dir.path()).is_err());
    }

    #[test]
    fn extract_tasks_returns_empty_when_no_task_table_exists() {
        let dir = TempDir::new("turbo-empty");
        fs::write(dir.path().join("turbo.json"), "{}").expect("turbo.json should be written");

        assert!(
            extract_tasks(dir.path())
                .expect("empty turbo config should parse")
                .is_empty()
        );
    }

    #[test]
    fn extract_tasks_reads_v2_tasks_schema() {
        let dir = TempDir::new("turbo-v2");
        fs::write(
            dir.path().join("turbo.json"),
            r#"{"tasks":{"build":{},"lint":{},"web#build":{}}}"#,
        )
        .expect("turbo.json should be written");

        let mut tasks = extract_tasks(dir.path()).expect("v2 turbo config should parse");
        tasks.sort_unstable();

        assert_eq!(tasks, ["build", "lint"]);
    }

    #[test]
    fn extract_tasks_reads_v1_pipeline_schema() {
        let dir = TempDir::new("turbo-v1");
        fs::write(
            dir.path().join("turbo.json"),
            r#"{"pipeline":{"test":{},"typecheck":{},"pkg#build":{}}}"#,
        )
        .expect("turbo.json should be written");

        let mut tasks = extract_tasks(dir.path()).expect("v1 turbo config should parse");
        tasks.sort_unstable();

        assert_eq!(tasks, ["test", "typecheck"]);
    }

    use super::is_self_passthrough;

    #[test]
    fn is_self_passthrough_matches_canonical_run_form() {
        assert!(is_self_passthrough("build", "turbo run build"));
    }

    #[test]
    fn is_self_passthrough_matches_with_trailing_flags() {
        assert!(is_self_passthrough(
            "build",
            "turbo run build --filter=web --concurrency=4"
        ));
    }

    #[test]
    fn is_self_passthrough_matches_shorthand_form() {
        assert!(is_self_passthrough("build", "turbo build"));
    }

    #[test]
    fn is_self_passthrough_tolerates_irregular_whitespace() {
        assert!(is_self_passthrough("build", "  turbo   run    build  "));
    }

    #[test]
    fn is_self_passthrough_rejects_real_script() {
        assert!(!is_self_passthrough("build", "vite build"));
    }

    #[test]
    fn is_self_passthrough_rejects_passthrough_to_different_target() {
        assert!(!is_self_passthrough("build", "turbo run lint"));
    }

    #[test]
    fn is_self_passthrough_rejects_indirect_invocation_via_pm() {
        // npx/pnpm exec wrappers are intentionally not matched.
        assert!(!is_self_passthrough("build", "npx turbo run build"));
        assert!(!is_self_passthrough("build", "pnpm exec turbo run build"));
    }

    #[test]
    fn is_self_passthrough_rejects_empty_or_partial_command() {
        assert!(!is_self_passthrough("build", ""));
        assert!(!is_self_passthrough("build", "turbo"));
        assert!(!is_self_passthrough("build", "turbo run"));
    }

    #[test]
    fn is_self_passthrough_rejects_shell_chain_and() {
        // `turbo run build && echo done` does extra work — not a thin
        // passthrough; swallowing it would hide the trailing command.
        assert!(!is_self_passthrough(
            "build",
            "turbo run build && echo done"
        ));
    }

    #[test]
    fn is_self_passthrough_rejects_shell_chain_or() {
        assert!(!is_self_passthrough("build", "turbo run build || exit 1"));
    }

    #[test]
    fn is_self_passthrough_rejects_shell_pipe() {
        assert!(!is_self_passthrough(
            "build",
            "turbo run build | tee log.txt"
        ));
    }

    #[test]
    fn is_self_passthrough_rejects_shell_redirect() {
        // `>` is a bare positional under split_whitespace, which falls into
        // the "non-flag, no flag-value expected" branch and rejects.
        assert!(!is_self_passthrough("build", "turbo run build > out.log"));
    }

    #[test]
    fn is_self_passthrough_rejects_shell_background() {
        assert!(!is_self_passthrough("build", "turbo run build &"));
    }

    #[test]
    fn is_self_passthrough_rejects_extra_positional_target() {
        // `turbo run build lint` runs both `build` and `lint` — invoking
        // through runner would silently drop `lint`, so don't classify
        // this as a passthrough.
        assert!(!is_self_passthrough("build", "turbo run build lint"));
    }

    #[test]
    fn is_self_passthrough_accepts_space_separated_flag_value() {
        assert!(is_self_passthrough("build", "turbo run build --filter web"));
    }

    #[test]
    fn is_self_passthrough_accepts_trailing_bool_flag() {
        // `--no-cache` takes no value; end-of-tokens exits the loop cleanly.
        assert!(is_self_passthrough("build", "turbo run build --no-cache"));
    }

    #[test]
    fn is_self_passthrough_rejects_stderr_to_stdout_after_flag() {
        // The bug CodeRabbit-reviewer #2 caught: `2>&1` was consumed as
        // `--no-cache`'s value. The redirect-detection pass now rejects.
        assert!(!is_self_passthrough(
            "build",
            "turbo run build --no-cache 2>&1"
        ));
    }

    #[test]
    fn is_self_passthrough_rejects_stdout_to_stderr_after_flag() {
        assert!(!is_self_passthrough(
            "build",
            "turbo run build --no-cache 1>&2"
        ));
    }

    #[test]
    fn is_self_passthrough_rejects_dev_null_redirect_after_flag() {
        assert!(!is_self_passthrough(
            "build",
            "turbo run build --no-cache 2>/dev/null"
        ));
    }

    #[test]
    fn is_self_passthrough_rejects_combined_fd_redirect_after_flag() {
        assert!(!is_self_passthrough(
            "build",
            "turbo run build --no-cache &>output.log"
        ));
    }

    #[test]
    fn is_self_passthrough_rejects_append_redirect_after_flag() {
        assert!(!is_self_passthrough(
            "build",
            "turbo run build --no-cache >>build.log"
        ));
    }

    #[test]
    fn is_self_passthrough_rejects_dup_fd_redirect_after_flag() {
        assert!(!is_self_passthrough(
            "build",
            "turbo run build --no-cache >&2"
        ));
    }

    #[test]
    fn is_self_passthrough_rejects_pipe_with_stderr_after_flag() {
        // `|&` (bash 4+ pipe both stdout and stderr) was previously
        // consumed as `--no-cache`'s value — now caught by the
        // expanded shell-control-token list.
        assert!(!is_self_passthrough(
            "build",
            "turbo run build --no-cache |& tee log"
        ));
    }

    #[test]
    fn is_self_passthrough_rejects_case_terminator_after_flag() {
        assert!(!is_self_passthrough(
            "build",
            "turbo run build --no-cache ;;"
        ));
    }

    #[test]
    fn is_self_passthrough_rejects_case_fallthrough_after_flag() {
        assert!(!is_self_passthrough(
            "build",
            "turbo run build --no-cache ;&"
        ));
        assert!(!is_self_passthrough(
            "build",
            "turbo run build --no-cache ;;&"
        ));
    }

    #[test]
    fn is_self_passthrough_rejects_negation_after_flag() {
        assert!(!is_self_passthrough(
            "build",
            "turbo run build --no-cache !"
        ));
    }

    #[test]
    fn is_self_passthrough_rejects_group_delimiters_after_flag() {
        assert!(!is_self_passthrough(
            "build",
            "turbo run build --no-cache {"
        ));
        assert!(!is_self_passthrough(
            "build",
            "turbo run build --no-cache }"
        ));
        assert!(!is_self_passthrough(
            "build",
            "turbo run build --no-cache ("
        ));
        assert!(!is_self_passthrough(
            "build",
            "turbo run build --no-cache )"
        ));
    }

    use super::{is_shell_control_token, looks_like_redirect};

    #[test]
    fn is_shell_control_token_matches_full_bash_set() {
        for op in [
            "&&", "||", ";", ";;", ";&", ";;&", "|", "|&", "&", "!", "{", "}", "(", ")",
        ] {
            assert!(
                is_shell_control_token(op),
                "expected `{op}` to be classified as shell control"
            );
        }
    }

    #[test]
    fn is_shell_control_token_rejects_flags_values_and_redirects() {
        // Anything that isn't an exact-match control op must not classify.
        for non_op in [
            "--filter", "web", "4", "@scope/*", "$(date)", ">", "2>&1", "&>",
        ] {
            assert!(
                !is_shell_control_token(non_op),
                "`{non_op}` must not classify as shell control"
            );
        }
    }

    #[test]
    fn looks_like_redirect_matches_bare_operators() {
        assert!(looks_like_redirect(">"));
        assert!(looks_like_redirect(">>"));
        assert!(looks_like_redirect("<"));
        assert!(looks_like_redirect("<<"));
        assert!(looks_like_redirect("<<<"));
    }

    #[test]
    fn looks_like_redirect_matches_combined_fd_forms() {
        assert!(looks_like_redirect("&>"));
        assert!(looks_like_redirect("&>>"));
        assert!(looks_like_redirect(">&"));
    }

    #[test]
    fn looks_like_redirect_matches_fd_prefixed_forms() {
        assert!(looks_like_redirect("2>"));
        assert!(looks_like_redirect("1>"));
        assert!(looks_like_redirect("3<"));
    }

    #[test]
    fn looks_like_redirect_matches_composite_forms() {
        assert!(looks_like_redirect("2>&1"));
        assert!(looks_like_redirect("1>&2"));
        assert!(looks_like_redirect("2>/dev/null"));
        assert!(looks_like_redirect("&>file.log"));
        assert!(looks_like_redirect(">file"));
        assert!(looks_like_redirect(">>append"));
    }

    #[test]
    fn looks_like_redirect_rejects_flags_and_values() {
        // Flags and ordinary values must not be misclassified as redirects.
        assert!(!looks_like_redirect("--filter"));
        assert!(!looks_like_redirect("--concurrency=4"));
        assert!(!looks_like_redirect("web"));
        assert!(!looks_like_redirect("4"));
        assert!(!looks_like_redirect("@scope/*"));
        assert!(!looks_like_redirect("$(date)"));
        // Quoted patterns: leading quote/letter, no `>` or `<` after fd-strip.
        assert!(!looks_like_redirect("'<pkg>'"));
        // Bare `&` is a metachar handled separately, not a redirect.
        assert!(!looks_like_redirect("&"));
    }
}
