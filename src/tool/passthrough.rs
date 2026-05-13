//! Detect `package.json` scripts that are thin wrappers around a task runner.
//!
//! A passthrough wrapper is a script whose entire body invokes a known task
//! runner with the same target name as the script itself, e.g. a
//! `"build": "just build"` entry whose only purpose is to expose the
//! `just` recipe under the package-manager script vocabulary.
//!
//! Detecting these lets the resolver (and shell completion) dedupe — when
//! a `"build"` script is just `just build` and a `justfile` already
//! exposes a real `build` recipe, listing both as separate candidates only
//! adds noise.
//!
//! Turborepo's specific case is detected by
//! [`crate::tool::turbo::is_self_passthrough`], which carries extensive
//! shell-token validation tuned for turbo's flag-rich invocations. This
//! module covers the simpler runners (`just`, `make`, `task`, `nx`,
//! `bacon`, `mise`) with a conservative one-shot matcher: binary,
//! optional `run` subcommand, same task name, no shell-active tail.

use crate::types::TaskRunner;

/// Detect whether `command` is a thin passthrough wrapper for `name`,
/// returning the task runner it dispatches to (if any).
///
/// The first match in this order wins, which matches the lockfile
/// priority of task runners elsewhere in detection:
///
/// 1. Turborepo (delegated to its specialized detector).
/// 2. just
/// 3. make
/// 4. go-task (`task <name>`)
/// 5. nx (`nx run <name>`)
/// 6. bacon
/// 7. mise (`mise run <name>`)
pub(crate) fn detect_target(name: &str, command: &str) -> Option<TaskRunner> {
    if crate::tool::turbo::is_self_passthrough(name, command) {
        return Some(TaskRunner::Turbo);
    }
    for (runner, binary, run_sub) in CANDIDATES {
        if simple_passthrough(name, command, binary, *run_sub) {
            return Some(*runner);
        }
    }
    None
}

/// Wrapper patterns for non-turbo runners — `(runner, binary,
/// run_subcommand)`. `nx` and `mise` use a `run <task>` shape; the rest
/// take the task name as the first positional.
const CANDIDATES: &[(TaskRunner, &str, Option<&str>)] = &[
    (TaskRunner::Just, "just", None),
    (TaskRunner::Make, "make", None),
    (TaskRunner::GoTask, "task", None),
    (TaskRunner::Nx, "nx", Some("run")),
    (TaskRunner::Bacon, "bacon", None),
    (TaskRunner::Mise, "mise", Some("run")),
];

/// Conservative passthrough matcher: requires `command` to be exactly
/// `<binary> [run_subcommand] <name> [args…]`, rejecting any tail that
/// contains a shell-active token.
///
/// The check is deliberately strict in the safe direction — false
/// negatives leave a script visible in completion as a separate
/// candidate, which is the same outcome we have today. False positives
/// would silently swallow a real script and need to be avoided.
fn simple_passthrough(
    name: &str,
    command: &str,
    binary: &str,
    run_subcommand: Option<&str>,
) -> bool {
    // Reject anything that spans multiple shell lines. `split_whitespace`
    // treats `\n` and `\r` as ordinary separators, so a script like
    // `"just build\necho owned"` would otherwise tokenise to
    // `["just", "build", "echo", "owned"]` and look like a thin
    // passthrough — the trailing `echo` is a separate command, not an
    // argument forwarded to `just`. Bash also accepts `\r\n` on Windows
    // editors so both characters get the early bail.
    //
    // Other control operators (`;`, `&&`, `||`, `|`) don't need an
    // early check: spaced forms surface as tokens that `is_shell_active`
    // rejects (substring `;`/`&`/`|`), and glued forms get rejected at
    // the binary/name token comparison or by the same any-position
    // substring scan in `is_shell_active`.
    if command.contains('\n') || command.contains('\r') {
        return false;
    }
    let mut tokens = command.split_whitespace();
    if tokens.next() != Some(binary) {
        return false;
    }
    if let Some(sub) = run_subcommand
        && tokens.next() != Some(sub)
    {
        return false;
    }
    if tokens.next() != Some(name) {
        return false;
    }
    tokens.all(|token| !is_shell_active(token))
}

/// Reject any token that introduces extra behavior beyond a thin
/// dispatch: shell control operators, redirects, parameter/command/
/// arithmetic expansion, backtick substitution.
///
/// Meta-characters are detected anywhere in the token (not just at the
/// start) so glued forms like `--watch&&echo` and `arg>out` are caught —
/// the shell tokenises those exactly as `--watch && echo` and
/// `arg > out` respectively, so a passthrough wrapper that contains
/// them is not actually a thin dispatch.
fn is_shell_active(token: &str) -> bool {
    // Expansion / substitution — `$VAR`, `$(cmd)`, `$((expr))`, `` `cmd` ``.
    if token.contains('$') || token.contains('`') {
        return true;
    }
    // Redirects (`>`, `<`, `>>`, `<<`, `>&`, `&>`, `1>foo`, `2>&1`, …)
    // and control operators (`&&`, `||`, `|`, `|&`, `;`, `;;`, `;&`,
    // backgrounding `cmd&`). Substring-matching `&` subsumes `&&`,
    // `>&`, `|&`, and trailing background; `|` subsumes `||` and
    // `|&`; `;` subsumes the compound forms. Any one of these in any
    // position means the shell will do real work, so we bail.
    if token
        .chars()
        .any(|c| matches!(c, '>' | '<' | '&' | '|' | ';'))
    {
        return true;
    }
    // Block / subshell delimiters are only meta when they appear as
    // standalone tokens — `{ a; b; }` requires whitespace separation
    // around `{` and `}`, and `( subshell )` likewise. Treating them
    // as substrings would over-reject benign args like
    // `--filter=name(v1)` that the shell would pass through verbatim,
    // so keep the exact-match check here.
    matches!(token, "!" | "{" | "}" | "(" | ")")
}

#[cfg(test)]
mod tests {
    use super::detect_target;
    use crate::types::TaskRunner;

    #[test]
    fn detects_just_passthrough() {
        assert_eq!(detect_target("build", "just build"), Some(TaskRunner::Just));
    }

    #[test]
    fn detects_make_passthrough() {
        assert_eq!(detect_target("test", "make test"), Some(TaskRunner::Make));
    }

    #[test]
    fn detects_go_task_passthrough() {
        assert_eq!(detect_target("lint", "task lint"), Some(TaskRunner::GoTask));
    }

    #[test]
    fn detects_nx_passthrough_with_run_subcommand() {
        assert_eq!(detect_target("build", "nx run build"), Some(TaskRunner::Nx));
    }

    #[test]
    fn detects_bacon_passthrough() {
        assert_eq!(
            detect_target("check", "bacon check"),
            Some(TaskRunner::Bacon)
        );
    }

    #[test]
    fn detects_mise_passthrough_with_run_subcommand() {
        assert_eq!(detect_target("ci", "mise run ci"), Some(TaskRunner::Mise));
    }

    #[test]
    fn rejects_when_target_name_mismatches() {
        // `just build` under a script named `dev` is doing real work — it
        // dispatches to a different recipe, not the same-named one.
        assert!(detect_target("dev", "just build").is_none());
    }

    #[test]
    fn rejects_when_script_body_starts_with_other_binary() {
        // `vite build` is a real build command, not a wrapper.
        assert!(detect_target("build", "vite build").is_none());
    }

    #[test]
    fn rejects_when_nx_run_subcommand_missing() {
        // `nx <name>` without `run` is an internal nx syntax we don't
        // treat as a passthrough wrapper — too easy to false-positive
        // on `nx serve` etc. when there's no same-named project.
        assert!(detect_target("build", "nx build").is_none());
    }

    #[test]
    fn rejects_when_tail_contains_pipe() {
        assert!(detect_target("test", "just test | tee log").is_none());
    }

    #[test]
    fn rejects_when_tail_contains_var_expansion() {
        assert!(detect_target("test", "just test $EXTRA_ARGS").is_none());
    }

    #[test]
    fn rejects_when_tail_contains_redirect() {
        assert!(detect_target("test", "just test > out.log").is_none());
    }

    #[test]
    fn rejects_when_tail_contains_command_substitution() {
        assert!(detect_target("test", "just test $(echo)").is_none());
    }

    #[test]
    fn rejects_when_tail_contains_glued_logical_and() {
        // No whitespace around `&&` — the shell still parses this as
        // `--watch && echo malicious`, so the wrapper isn't actually a
        // thin dispatch.
        assert!(detect_target("test", "just test --watch&&echo done").is_none());
    }

    #[test]
    fn rejects_when_tail_contains_glued_logical_or() {
        assert!(detect_target("test", "just test --watch||fallback").is_none());
    }

    #[test]
    fn rejects_when_tail_contains_glued_pipe() {
        assert!(detect_target("test", "just test --report|tee").is_none());
    }

    #[test]
    fn rejects_when_tail_contains_glued_semicolon() {
        assert!(detect_target("test", "just test foo;echo done").is_none());
    }

    #[test]
    fn rejects_when_tail_contains_glued_redirect() {
        // Arg ending in `>file` is a redirect, not an argument value.
        assert!(detect_target("test", "just test arg>out.log").is_none());
    }

    #[test]
    fn rejects_when_tail_contains_glued_input_redirect() {
        assert!(detect_target("test", "just test arg<input.txt").is_none());
    }

    #[test]
    fn rejects_when_tail_contains_glued_fd_redirect() {
        // `2>&1` and `2>file` glued onto an arg.
        assert!(detect_target("test", "just test arg2>&1").is_none());
    }

    #[test]
    fn rejects_when_tail_contains_glued_background() {
        // Trailing `&` makes the command run in the background — not
        // a passthrough.
        assert!(detect_target("test", "just test arg&").is_none());
    }

    #[test]
    fn rejects_when_body_contains_newline() {
        // Multi-line scripts are NOT thin passthroughs even if the
        // first line happens to look like one — the second line is a
        // separate command. `split_whitespace` would otherwise
        // flatten the newline and let the trailing `echo owned`
        // masquerade as forwarded args.
        assert!(detect_target("build", "just build\necho owned").is_none());
    }

    #[test]
    fn rejects_when_body_contains_carriage_return() {
        // `\r\n` line endings (Windows editors) get the same
        // treatment as `\n` — bash treats `\r` as a token separator
        // that can hide multi-line content.
        assert!(detect_target("build", "just build\r\necho owned").is_none());
    }

    #[test]
    fn rejects_when_body_is_multiline_block() {
        // The whole tail could be a heredoc-style block. Reject on the
        // first newline regardless of what follows.
        let body = "just build\nif [ $? -ne 0 ]; then\n  exit 1\nfi";
        assert!(detect_target("build", body).is_none());
    }

    #[test]
    fn accepts_when_tail_is_plain_flags_only() {
        // Plain `--watch` is fine — it's just an arg forwarded to the
        // underlying runner, no shell action.
        assert_eq!(
            detect_target("test", "just test --watch"),
            Some(TaskRunner::Just)
        );
    }

    #[test]
    fn turbo_passthrough_still_routes_to_turbo_runner() {
        assert_eq!(
            detect_target("build", "turbo run build"),
            Some(TaskRunner::Turbo)
        );
        assert_eq!(
            detect_target("build", "turbo build"),
            Some(TaskRunner::Turbo)
        );
    }
}
