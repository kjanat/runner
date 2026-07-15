//! Turborepo, monorepo build system.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Context as _;
use serde::Deserialize;

use crate::tool::files;

/// Directories produced by Turborepo.
pub(crate) const CLEAN_DIRS: &[&str] = &[".turbo"];

/// Supported Turborepo config filenames (priority order). Turborepo v2
/// accepts both `turbo.json` and `turbo.jsonc` natively.
pub(crate) const FILENAMES: &[&str] = &["turbo.json", "turbo.jsonc"];

/// Resolve the active Turborepo config in `dir`, if any.
pub(crate) fn find_config(dir: &Path) -> Option<PathBuf> {
    files::find_first(dir, FILENAMES).filter(|path| path.is_file())
}

/// Detected via `turbo.json` or `turbo.jsonc`.
pub(crate) fn detect(dir: &Path) -> bool {
    find_config(dir).is_some()
}

/// Parse task names from `turbo.json` / `turbo.jsonc`.
///
/// Supports both v2 (`"tasks"`) and v1 (`"pipeline"`) schemas. Workspace-
/// scoped entries like `"my-app#build"` are filtered out, while Root Task
/// entries (`"//#lint"`) are surfaced as their bare name (`"lint"`), both
/// are invoked the same way as a plain task. JSONC syntax (line comments,
/// block comments, trailing commas) is accepted under either filename,
/// matching Turborepo's own parser.
pub(crate) fn extract_tasks(dir: &Path) -> anyhow::Result<Vec<String>> {
    #[derive(Deserialize)]
    struct Partial {
        tasks: Option<HashMap<String, serde_json::Value>>,
        pipeline: Option<HashMap<String, serde_json::Value>>,
    }
    let Some(path) = find_config(dir) else {
        return Ok(vec![]);
    };
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let p = json5::from_str::<Partial>(&content)
        .with_context(|| format!("{} is not valid JSON/JSONC", path.display()))?;
    let Some(tasks) = p.tasks.or(p.pipeline) else {
        return Ok(vec![]);
    };
    Ok(tasks
        .into_keys()
        .filter_map(classify_task_key)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect())
}

/// Map a raw `tasks`/`pipeline` key to the runnable task name, or drop it.
///
/// - `"//#lint"` → `Some("lint")` (Root Task, surface bare name).
/// - `"web#build"` → `None` (workspace-scoped, invoked as `web#build`,
///   not as a top-level task).
/// - `"build"` → `Some("build")` (plain task).
/// - `"//#"` or `"//#a#b"` → `None` (malformed).
fn classify_task_key(name: String) -> Option<String> {
    if let Some(rest) = name.strip_prefix("//#") {
        return (!rest.is_empty() && !rest.contains('#')).then(|| rest.to_string());
    }
    (!name.contains('#')).then_some(name)
}

/// `turbo run <task> [-- args...]`
pub(crate) fn run_cmd(task: &str, args: &[String]) -> Command {
    let mut c = super::program::command("turbo");
    c.arg("run").arg(task);
    if !args.is_empty() {
        c.arg("--").args(args);
    }
    c
}

/// Returns `true` if `command` is a thin invocation of `turbo` that targets
/// the same task name, i.e. `turbo run <name>` or the shorthand
/// `turbo <name>`, optionally followed by flag tokens (e.g. `--filter web`,
/// `--concurrency=4`).
///
/// The tail after the target name must be flag tokens (`-x`, `--long`,
/// `--key=value`), values following a non-`=` flag, or args after a
/// bare `--` end-of-options separator (turbo's `turbo run <task> --
/// <args...>` forwarding pattern). Any shell control operator, redirect,
/// or expansion token (`$`/backtick) rejects the match, they mean the
/// script does more than dispatch to turbo.
///
/// Purely a textual heuristic on the script body. Indirect invocations
/// (`npx turbo run build`, `pnpm exec …`) are deliberately not matched
/// to avoid false positives on unrelated wrapper scripts. Errs toward
/// false negatives (leaving a script visible) on ambiguous tails like
/// quoted multi-word args or unquoted globs.
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

    // Tail must be flags-only (with optional space-separated values),
    // or a `--` end-of-options marker followed by forwarded args.
    // Any bare positional, shell metachar, redirect operator, or
    // shell-expansion token means the script is doing extra work
    // and is not a thin passthrough.
    //
    // Order matters: control / redirect / expansion checks all run
    // BEFORE flag-value consumption so that tokens like `2>&1`,
    // `|&`, or `$X` positioned after a value-expecting flag are
    // rejected, not silently swallowed as the flag's value.
    //
    // `--` (POSIX end-of-options separator): turbo forwards everything
    // after `--` to the underlying task. Once seen, remaining tokens
    // are accepted as forwarded args (still rejecting shell metachars,
    // redirects, and expansions which would mean the script body does
    // real work beyond dispatching to turbo).
    let mut expects_flag_value = false;
    let mut after_double_dash = false;
    for token in tokens {
        if is_shell_control_token(token) {
            return false;
        }
        if looks_like_redirect(token) {
            return false;
        }
        if looks_like_shell_expansion(token) {
            return false;
        }
        if after_double_dash {
            continue;
        }
        if token == "--" {
            after_double_dash = true;
            continue;
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

/// Detects tokens that perform shell expansion: parameter expansion,
/// command substitution, or arithmetic. Any of these mean the script's
/// effective behavior depends on shell state at run time, so it isn't
/// a thin turbo dispatch.
///
/// Catches the full bash expansion family in one rule: presence of
/// `$` covers `$X`, `${X}`, `${X:-def}`, `${X//a/b}`, `${!X}`,
/// `${#X}`, `${X[@]}`, special vars (`$@`, `$*`, `$#`, `$?`, `$$`,
/// `$!`, `$_`, `$0`-`$9`, `${10}+`), `$(cmd)`, `$((expr))`, and any
/// double-quoted form with embedded expansion (`"${X}"`). Backtick
/// covers the legacy `` `cmd` `` substitution form.
fn looks_like_shell_expansion(token: &str) -> bool {
    token.contains('$') || token.contains('`')
}

#[cfg(test)]
#[allow(
    clippy::literal_string_with_formatting_args,
    reason = "test fixtures embed bash parameter-expansion strings like `${X:-default}` and \
              `${X:+alt}` as input to is_self_passthrough; the lint mistakes them for Rust format \
              args."
)]
mod tests {
    use std::fs;

    use super::{detect, extract_tasks};
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

    #[test]
    fn detect_finds_turbo_jsonc_filename() {
        let dir = TempDir::new("turbo-detect-jsonc");
        fs::write(dir.path().join("turbo.jsonc"), r#"{"tasks":{"build":{}}}"#)
            .expect("turbo.jsonc should be written");

        assert!(detect(dir.path()));
    }

    #[test]
    fn extract_tasks_reads_turbo_jsonc_filename() {
        let dir = TempDir::new("turbo-jsonc-filename");
        fs::write(
            dir.path().join("turbo.jsonc"),
            r#"{"tasks":{"build":{},"lint":{}}}"#,
        )
        .expect("turbo.jsonc should be written");

        let mut tasks = extract_tasks(dir.path()).expect("turbo.jsonc should parse");
        tasks.sort_unstable();

        assert_eq!(tasks, ["build", "lint"]);
    }

    #[test]
    fn extract_tasks_accepts_trailing_commas_in_turbo_json() {
        let dir = TempDir::new("turbo-trailing-comma");
        fs::write(dir.path().join("turbo.json"), r#"{"tasks":{"build":{},}}"#)
            .expect("turbo.json should be written");

        let tasks = extract_tasks(dir.path()).expect("trailing comma should parse");

        assert_eq!(tasks, ["build"]);
    }

    #[test]
    fn extract_tasks_accepts_jsonc_comments_under_either_filename() {
        let dir = TempDir::new("turbo-jsonc-comments");
        fs::write(
            dir.path().join("turbo.jsonc"),
            r#"{
  // line comment
  "tasks": {
    /* block comment */
    "build": {},
    "test": {},
  },
}
"#,
        )
        .expect("turbo.jsonc should be written");

        let mut tasks = extract_tasks(dir.path()).expect("jsonc should parse");
        tasks.sort_unstable();

        assert_eq!(tasks, ["build", "test"]);
    }

    #[test]
    fn extract_tasks_surfaces_root_tasks_with_bare_name() {
        let dir = TempDir::new("turbo-root-tasks");
        fs::write(
            dir.path().join("turbo.json"),
            r#"{"tasks":{"//#lint":{},"//#format":{"cache":false}}}"#,
        )
        .expect("turbo.json should be written");

        let mut tasks = extract_tasks(dir.path()).expect("root tasks should parse");
        tasks.sort_unstable();

        assert_eq!(tasks, ["format", "lint"]);
    }

    #[test]
    fn extract_tasks_mixes_root_plain_and_workspace_scoped_entries() {
        // The bug-report repro: root tasks must surface, plain tasks pass
        // through, workspace-scoped (`web#build`) stays filtered.
        let dir = TempDir::new("turbo-mixed-keys");
        fs::write(
            dir.path().join("turbo.json"),
            r#"{"tasks":{"build":{},"//#lint":{},"//#format":{"cache":false},"web#build":{}}}"#,
        )
        .expect("turbo.json should be written");

        let mut tasks = extract_tasks(dir.path()).expect("mixed keys should parse");
        tasks.sort_unstable();

        assert_eq!(tasks, ["build", "format", "lint"]);
    }

    #[test]
    fn extract_tasks_drops_malformed_root_task_keys() {
        // `//#` with no name and `//#a#b` (extra `#`) are not valid root
        // tasks, drop them rather than surface a confusing entry.
        let dir = TempDir::new("turbo-malformed-root");
        fs::write(
            dir.path().join("turbo.json"),
            r#"{"tasks":{"//#":{},"//#a#b":{},"//#ok":{}}}"#,
        )
        .expect("turbo.json should be written");

        let tasks = extract_tasks(dir.path()).expect("malformed root keys should parse");

        assert_eq!(tasks, ["ok"]);
    }

    #[test]
    fn extract_tasks_dedupes_root_task_and_plain_task_collision() {
        // Both `lint` and `//#lint` resolve to the same `turbo run lint`
        // invocation, so listing both would render a duplicate row.
        let dir = TempDir::new("turbo-root-collision");
        fs::write(
            dir.path().join("turbo.json"),
            r#"{"tasks":{"lint":{},"//#lint":{}}}"#,
        )
        .expect("turbo.json should be written");

        let tasks = extract_tasks(dir.path()).expect("colliding keys should parse");

        assert_eq!(tasks, ["lint"]);
    }

    #[test]
    fn extract_tasks_prefers_turbo_json_when_both_filenames_exist() {
        let dir = TempDir::new("turbo-priority");
        fs::write(
            dir.path().join("turbo.json"),
            r#"{"tasks":{"from-json":{}}}"#,
        )
        .expect("turbo.json should be written");
        fs::write(
            dir.path().join("turbo.jsonc"),
            r#"{"tasks":{"from-jsonc":{}}}"#,
        )
        .expect("turbo.jsonc should be written");

        let tasks = extract_tasks(dir.path()).expect("turbo.json should parse");

        assert_eq!(tasks, ["from-json"]);
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
        // `turbo run build && echo done` does extra work, not a thin
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
        // `turbo run build lint` runs both `build` and `lint`, invoking
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
        // `2>&1` must not be consumed as `--no-cache`'s value; the
        // redirect-detection pass rejects it.
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
        // consumed as `--no-cache`'s value, now caught by the
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

    #[test]
    fn is_self_passthrough_rejects_var_expansion_after_flag() {
        // `--filter $X` was previously consumed: the value of `$X` is
        // resolved at run time, so the script's effective filter
        // depends on shell state, not a thin passthrough.
        assert!(!is_self_passthrough("build", "turbo run build --filter $X"));
    }

    #[test]
    fn is_self_passthrough_rejects_braced_var_after_flag() {
        assert!(!is_self_passthrough(
            "build",
            "turbo run build --filter ${X}"
        ));
    }

    #[test]
    fn is_self_passthrough_rejects_default_var_after_flag() {
        assert!(!is_self_passthrough(
            "build",
            "turbo run build --filter ${X:-web}"
        ));
    }

    #[test]
    fn is_self_passthrough_rejects_pattern_substitution_after_flag() {
        assert!(!is_self_passthrough(
            "build",
            "turbo run build --filter ${X//foo/bar}"
        ));
    }

    #[test]
    fn is_self_passthrough_rejects_command_substitution_after_flag() {
        assert!(!is_self_passthrough(
            "build",
            "turbo run build --filter $(get_filter)"
        ));
    }

    #[test]
    fn is_self_passthrough_rejects_backtick_substitution_after_flag() {
        assert!(!is_self_passthrough(
            "build",
            "turbo run build --filter `get_filter`"
        ));
    }

    #[test]
    fn is_self_passthrough_rejects_arithmetic_expansion_after_flag() {
        // Whitespace in `$((CORES * 2))` splits into multiple tokens,
        // but the first one (`$((CORES`) still trips the `$` check.
        assert!(!is_self_passthrough(
            "build",
            "turbo run build --concurrency $((CORES * 2))"
        ));
        // Same form without internal spaces.
        assert!(!is_self_passthrough(
            "build",
            "turbo run build --concurrency $((CORES*2))"
        ));
    }

    #[test]
    fn is_self_passthrough_rejects_special_var_after_flag() {
        assert!(!is_self_passthrough("build", "turbo run build --filter $@"));
        assert!(!is_self_passthrough("build", "turbo run build --filter $*"));
    }

    #[test]
    fn is_self_passthrough_rejects_quoted_expansion_after_flag() {
        // The exact form from the user's bug report:
        // `"build": "turbo run build \"${X}\""` decodes to
        // `turbo run build "${X}"`, the quoted form must reject too.
        assert!(!is_self_passthrough(
            "build",
            "turbo run build --filter \"${X}\""
        ));
        // Standalone positional case (already rejected via the
        // positional rule, but pin behavior under the new rule too).
        assert!(!is_self_passthrough("build", "turbo run build \"${X}\""));
    }

    #[test]
    fn is_self_passthrough_rejects_bare_var_positional() {
        // Standalone `$X` after the target, already rejected by the
        // positional rule; under the new rule it now rejects via the
        // explicit shell-expansion check, which is clearer.
        assert!(!is_self_passthrough("build", "turbo run build $X"));
    }

    use super::{is_shell_control_token, looks_like_redirect, looks_like_shell_expansion};

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
    fn looks_like_shell_expansion_matches_full_dollar_family() {
        for form in [
            // Parameter expansion variants.
            "$X",
            "${X}",
            "${X:-default}",
            "${X:=default}",
            "${X:?msg}",
            "${X:+alt}",
            "${X#prefix}",
            "${X##prefix}",
            "${X%suffix}",
            "${X%%suffix}",
            "${X/foo/bar}",
            "${X//foo/bar}",
            "${X^^}",
            "${X,,}",
            "${#X}",
            "${!X}",
            "${X[@]}",
            "${X[0]}",
            // Special vars.
            "$@",
            "$*",
            "$#",
            "$?",
            "$$",
            "$!",
            "$_",
            "$0",
            "$9",
            "${10}",
            // Command substitution.
            "$(cmd)",
            "$(cmd --flag)",
            // Arithmetic expansion.
            "$((1+1))",
            "$((CORES*2))",
            // Quoted forms with embedded expansion.
            "\"${X}\"",
            "\"$X\"",
            "\"prefix-${X}\"",
        ] {
            assert!(
                looks_like_shell_expansion(form),
                "expected `{form}` to be detected as shell expansion"
            );
        }
    }

    #[test]
    fn looks_like_shell_expansion_matches_backtick_substitution() {
        assert!(looks_like_shell_expansion("`cmd`"));
        assert!(looks_like_shell_expansion("`cmd --flag`"));
        assert!(looks_like_shell_expansion("prefix-`cmd`-suffix"));
    }

    #[test]
    fn looks_like_shell_expansion_rejects_plain_values_and_flags() {
        for plain in [
            "--filter",
            "--concurrency=4",
            "web",
            "4",
            "@scope/*",
            "./packages/web",
            ">",
            "2>&1",
            "&>",
            "{a,b,c}",
            "~/cache",
        ] {
            assert!(
                !looks_like_shell_expansion(plain),
                "`{plain}` must not classify as shell expansion"
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

    #[test]
    fn is_self_passthrough_accepts_double_dash_with_forwarded_flag() {
        // POSIX `--` end-of-options separator: turbo forwards everything
        // after `--` to the underlying task.
        assert!(is_self_passthrough("build", "turbo run build -- --watch"));
        assert!(is_self_passthrough(
            "test",
            "turbo run test --filter web -- --reporter=verbose"
        ));
    }

    #[test]
    fn is_self_passthrough_accepts_double_dash_with_multiple_positionals() {
        // Multi-positional forwarding via `--` was the concrete bug:
        // pre-fix, `arg2` rejected as a bare positional because `--`
        // had been treated as a value-expecting flag.
        assert!(is_self_passthrough("build", "turbo run build -- arg1 arg2"));
        assert!(is_self_passthrough(
            "build",
            "turbo run build -- arg1 arg2 arg3 arg4"
        ));
    }

    #[test]
    fn is_self_passthrough_accepts_double_dash_with_no_following_args() {
        assert!(is_self_passthrough("build", "turbo run build --"));
    }

    #[test]
    fn is_self_passthrough_rejects_shell_chain_after_double_dash() {
        // Shell metacharacters after `--` still mean the script does
        // real work; the separator is not a free pass.
        assert!(!is_self_passthrough(
            "build",
            "turbo run build -- --watch && echo done"
        ));
        assert!(!is_self_passthrough(
            "build",
            "turbo run build -- arg1 ; cleanup"
        ));
    }

    #[test]
    fn is_self_passthrough_rejects_redirect_after_double_dash() {
        assert!(!is_self_passthrough(
            "build",
            "turbo run build -- > build.log"
        ));
        assert!(!is_self_passthrough(
            "build",
            "turbo run build -- --watch 2>&1"
        ));
    }

    #[test]
    fn is_self_passthrough_rejects_expansion_after_double_dash() {
        assert!(!is_self_passthrough("build", "turbo run build -- $TARGET"));
        assert!(!is_self_passthrough(
            "build",
            "turbo run build -- --filter ${SCOPE}"
        ));
        assert!(!is_self_passthrough(
            "build",
            "turbo run build -- $(date +%s)"
        ));
    }
}
