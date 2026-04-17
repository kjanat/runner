//! Custom shell completion adapters.
//!
//! Provides a zsh adapter that groups completions by
//! [`CompletionCandidate::tag`] so the shell renders section headers
//! (e.g. `-- justfile --`, `-- Commands --`).

use std::ffi::OsString;

use clap::ValueHint;
use clap_complete::env::{Bash, Elvish, EnvCompleter, Fish, Powershell, Shells};

/// Sentinel line emitted by the zsh adapter when the current argument
/// position wants shell-native path completion (so zsh can handle `~`,
/// named directories, globbing, `cdpath`, etc.). Format:
/// `__CLAP_PATHFILES__<TAB><flags>` where `<flags>` is forwarded verbatim
/// to zsh's `_files` builtin (e.g. `-/` for directories-only).
const PATHFILES_SENTINEL: &str = "__CLAP_PATHFILES__";

/// Shell completers with tag-grouped zsh output.
pub(crate) const SHELLS: Shells<'static> =
    Shells(&[&Bash, &Elvish, &Fish, &Powershell, &GroupedZsh]);

/// Tag-aware zsh adapter.
///
/// Emits `TAG\x1fVALUE\tDESCRIPTION` lines from [`write_complete`] and
/// generates a registration script that groups completions under separate
/// `compadd -V` calls per tag — producing `-- tag --` section headers.
struct GroupedZsh;

impl EnvCompleter for GroupedZsh {
    fn name(&self) -> &'static str {
        "zsh"
    }

    fn is(&self, name: &str) -> bool {
        name == "zsh"
    }

    fn write_registration(
        &self,
        var: &str,
        name: &str,
        bin: &str,
        completer: &str,
        buf: &mut dyn std::io::Write,
    ) -> Result<(), std::io::Error> {
        let escaped_name = name.replace('-', "_");
        let bin = shlex::try_quote(bin).unwrap_or(std::borrow::Cow::Borrowed(bin));
        let completer =
            shlex::try_quote(completer).unwrap_or(std::borrow::Cow::Borrowed(completer));

        let script = include_str!("grouped.zsh")
            .replace("{NAME}", &escaped_name)
            .replace("{COMPLETER}", &completer)
            .replace("{BIN}", &bin)
            .replace("{VAR}", var);

        writeln!(buf, "{script}")?;
        Ok(())
    }

    fn write_complete(
        &self,
        cmd: &mut clap::Command,
        args: Vec<OsString>,
        current_dir: Option<&std::path::Path>,
        buf: &mut dyn std::io::Write,
    ) -> Result<(), std::io::Error> {
        let index: usize = std::env::var("_CLAP_COMPLETE_INDEX")
            .ok()
            .and_then(|i| i.parse().ok())
            .unwrap_or_default();
        let ifs: Option<String> = std::env::var("_CLAP_IFS").ok().and_then(|i| i.parse().ok());

        let mut args = args;
        if args.len() == index {
            args.push(OsString::new());
        }

        // Short-circuit when the current position is a path-typed flag value:
        // emit a sentinel so the zsh script can delegate to its native
        // `_files` builtin (which understands `~`, named dirs, `cdpath`,
        // globs — all things clap's Rust-side path lister doesn't know).
        if let Some(flags) = detect_path_files_flags(cmd, &args, index) {
            write!(buf, "{PATHFILES_SENTINEL}\t{flags}")?;
            return Ok(());
        }

        let completions = clap_complete::engine::complete(cmd, args, index, current_dir)?;

        for (i, candidate) in completions.iter().enumerate() {
            if i != 0 {
                write!(buf, "{}", ifs.as_deref().unwrap_or("\n"))?;
            }
            let tag = candidate
                .get_tag()
                .map_or_else(|| "values".to_string(), ToString::to_string);

            // Format: TAG \x1f VALUE [\t DESCRIPTION]
            // \x1f separates tag from entry, \t separates value from description.
            // Using \t instead of : avoids the need for \: escaping in values
            // like "package.json:test".
            write!(
                buf,
                "{}\x1f{}",
                tag,
                candidate.get_value().to_string_lossy()
            )?;
            if let Some(help) = candidate.get_help() {
                let raw = help.to_string();
                let line = raw.lines().next().unwrap_or_default();
                let stripped = strip_tag_prefix(line, &tag);
                if !stripped.is_empty() {
                    write!(buf, "\t{stripped}")?;
                }
            }
        }
        Ok(())
    }
}

/// Strip a leading `"TAG: "` or `"TAG"` prefix from help text when it
/// matches the completion group tag (avoids redundancy in grouped output).
fn strip_tag_prefix<'a>(help: &'a str, tag: &str) -> &'a str {
    help.strip_prefix(tag)
        .map_or(help, |rest| rest.strip_prefix(": ").unwrap_or(rest))
        .trim()
}

/// If the token at `index` is the value of a path-typed long flag (either
/// `--flag=<value>` in a single token or `--flag` followed by a value
/// token), return the `_files` flag string zsh should use. Otherwise return
/// `None`, leaving completion to clap's engine.
fn detect_path_files_flags(
    cmd: &clap::Command,
    args: &[OsString],
    index: usize,
) -> Option<&'static str> {
    let current = args.get(index)?.to_string_lossy();
    let chain = active_command_chain(cmd, args, index);

    // `--flag=value` — the current token carries both, we're completing `value`.
    if let Some((flag, _value)) = current.split_once('=')
        && let Some(long) = flag.strip_prefix("--")
        && let Some(hint) = find_long_value_hint(&chain, long)
    {
        return zsh_files_flags(hint);
    }

    // Previous token is `--flag` (no `=`), current token is its value.
    if index > 0 {
        let prev = args[index - 1].to_string_lossy();
        if let Some(long) = prev.strip_prefix("--")
            && !long.contains('=')
            && let Some(hint) = find_long_value_hint(&chain, long)
        {
            return zsh_files_flags(hint);
        }
    }

    None
}

/// Walk `args[1..index]` and descend into matching subcommands to build the
/// active command chain (root first, deepest last). Stops as soon as a
/// positional argument fails to match any subcommand of the current node —
/// that's where positionals for the leaf command begin. Leading options
/// and their values are skipped.
fn active_command_chain<'a>(
    root: &'a clap::Command,
    args: &[OsString],
    index: usize,
) -> Vec<&'a clap::Command> {
    let mut chain = vec![root];
    let mut current = root;
    let mut i = 1;
    let stop = index.min(args.len());
    while i < stop {
        let token = args[i].to_string_lossy();
        if token == "--" {
            break;
        }
        if token.starts_with("--") {
            // `--flag=value` consumes one token; `--flag value` consumes two
            // when the flag expects a value on any command in the active
            // chain (so a global flag like `--dir`, defined on root, is
            // still recognised after descending into a subcommand).
            if !token.contains('=')
                && let Some(long) = token.strip_prefix("--")
                && long_flag_takes_value(&chain, long)
            {
                i += 2;
                continue;
            }
            i += 1;
            continue;
        }
        if token.starts_with('-') && token.len() > 1 {
            // Short flag cluster. We don't use short flags taking values,
            // but be conservative and just skip the cluster.
            i += 1;
            continue;
        }
        if let Some(sub) = current.find_subcommand(token.as_ref()) {
            chain.push(sub);
            current = sub;
            i += 1;
        } else {
            // First positional that isn't a subcommand — we've hit the
            // leaf command's own positionals (task name, etc).
            break;
        }
    }
    chain
}

/// Whether the long option `name` consumes a following positional as its
/// value, using the same deepest-first shadowing rule as
/// [`find_long_value_hint`]: the subcommand-local definition wins over an
/// ancestor's. This matters when a subcommand reuses a root flag name
/// with a different [`clap::ArgAction`] (e.g. root defines `--flag <VALUE>`
/// and a subcommand redeclares `--flag` as a boolean) — the walker must
/// honour the leaf command's semantics.
fn long_flag_takes_value(chain: &[&clap::Command], name: &str) -> bool {
    chain
        .iter()
        .rev()
        .find_map(|cmd| {
            cmd.get_arguments()
                .find(|arg| arg.get_long() == Some(name))
                .map(|arg| {
                    !matches!(
                        arg.get_action(),
                        clap::ArgAction::SetTrue
                            | clap::ArgAction::SetFalse
                            | clap::ArgAction::Count
                            | clap::ArgAction::Help
                            | clap::ArgAction::Version
                            | clap::ArgAction::HelpShort
                            | clap::ArgAction::HelpLong
                    )
                })
        })
        .unwrap_or(false)
}

/// Search the active command chain (deepest first, so a subcommand-local
/// definition shadows the root) for a long arg named `name`.
fn find_long_value_hint(chain: &[&clap::Command], name: &str) -> Option<ValueHint> {
    for cmd in chain.iter().rev() {
        for arg in cmd.get_arguments() {
            if arg.get_long() == Some(name) {
                return Some(arg.get_value_hint());
            }
        }
    }
    None
}

/// Map a clap [`ValueHint`] to the flag string used with zsh's `_files`.
/// Returns `None` for hints that aren't path-like (so regular clap
/// completion keeps running).
const fn zsh_files_flags(hint: ValueHint) -> Option<&'static str> {
    match hint {
        ValueHint::DirPath => Some("-/"),
        ValueHint::FilePath | ValueHint::AnyPath | ValueHint::ExecutablePath => Some(""),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use clap::{Arg, Command, ValueHint};

    use super::{detect_path_files_flags, strip_tag_prefix};

    fn dir_flag_cmd() -> Command {
        Command::new("runner").arg(
            Arg::new("dir")
                .long("dir")
                .value_hint(ValueHint::DirPath)
                .num_args(1),
        )
    }

    fn to_os(strings: &[&str]) -> Vec<OsString> {
        strings.iter().map(|s| OsString::from(*s)).collect()
    }

    #[test]
    fn detect_path_files_recognises_separated_dir_flag() {
        let cmd = dir_flag_cmd();
        let args = to_os(&["runner", "--dir", ""]);

        assert_eq!(detect_path_files_flags(&cmd, &args, 2), Some("-/"));
    }

    #[test]
    fn detect_path_files_recognises_inline_equals_dir_flag() {
        let cmd = dir_flag_cmd();
        let args = to_os(&["runner", "--dir=~/pro"]);

        assert_eq!(detect_path_files_flags(&cmd, &args, 1), Some("-/"));
    }

    /// A root-level value-taking flag must still be recognised by the
    /// chain walker after descending into a subcommand, so the walker
    /// correctly consumes its value token and the hint lookup returns
    /// the root's [`ValueHint`].
    #[test]
    fn detect_path_files_recognises_root_flag_after_subcommand() {
        let cmd = Command::new("runner")
            .arg(
                Arg::new("dir")
                    .long("dir")
                    .value_hint(ValueHint::DirPath)
                    .num_args(1),
            )
            .subcommand(Command::new("run"));

        let args = to_os(&["runner", "run", "--dir", ""]);
        assert_eq!(detect_path_files_flags(&cmd, &args, 3), Some("-/"));
    }

    /// Subcommand-local redefinition shadows a root flag: if root's
    /// `--flag` takes a value but the subcommand redeclares it as a
    /// boolean, the walker must not consume the next token as a value
    /// once we're inside that subcommand.
    #[test]
    fn detect_path_files_honours_boolean_shadow_on_subcommand() {
        let cmd = Command::new("runner")
            .arg(
                Arg::new("flag")
                    .long("flag")
                    .value_hint(ValueHint::DirPath)
                    .num_args(1),
            )
            .subcommand(
                Command::new("leaf").arg(
                    Arg::new("flag")
                        .long("flag")
                        .action(clap::ArgAction::SetTrue),
                ),
            );

        // Inside `leaf`, `--flag` is a boolean: the next token is the
        // positional we're completing, not `--flag`'s value, so no path
        // sentinel should be emitted.
        let args = to_os(&["runner", "leaf", "--flag", ""]);
        assert_eq!(detect_path_files_flags(&cmd, &args, 3), None);
    }

    /// Two sibling subcommands each define the same long flag with
    /// different [`ValueHint`]s; the lookup should pick the one on the
    /// subcommand the user is actually in.
    #[test]
    fn detect_path_files_respects_active_subcommand() {
        let cmd = Command::new("runner")
            .subcommand(
                Command::new("build").arg(
                    Arg::new("out")
                        .long("out")
                        .value_hint(ValueHint::DirPath)
                        .num_args(1),
                ),
            )
            .subcommand(
                Command::new("deploy").arg(
                    Arg::new("out")
                        .long("out")
                        .value_hint(ValueHint::FilePath)
                        .num_args(1),
                ),
            );

        let build_args = to_os(&["runner", "build", "--out", ""]);
        assert_eq!(
            detect_path_files_flags(&cmd, &build_args, 3),
            Some("-/"),
            "build's DirPath should win in build context"
        );

        let deploy_args = to_os(&["runner", "deploy", "--out", ""]);
        assert_eq!(
            detect_path_files_flags(&cmd, &deploy_args, 3),
            Some(""),
            "deploy's FilePath should win in deploy context"
        );
    }

    #[test]
    fn detect_path_files_walks_subcommands() {
        let cmd = Command::new("runner").subcommand(
            Command::new("run").arg(
                Arg::new("target")
                    .long("target")
                    .value_hint(ValueHint::FilePath)
                    .num_args(1),
            ),
        );
        let args = to_os(&["runner", "run", "--target", ""]);

        assert_eq!(detect_path_files_flags(&cmd, &args, 3), Some(""));
    }

    #[test]
    fn detect_path_files_ignores_non_path_flags() {
        let cmd = Command::new("runner").arg(Arg::new("name").long("name").num_args(1));
        let args = to_os(&["runner", "--name", ""]);

        assert_eq!(detect_path_files_flags(&cmd, &args, 2), None);
    }

    #[test]
    fn strip_tag_prefix_removes_matching_source() {
        assert_eq!(
            strip_tag_prefix("justfile: Format code", "justfile"),
            "Format code"
        );
    }

    #[test]
    fn strip_tag_prefix_leaves_non_matching_help_unchanged() {
        assert_eq!(strip_tag_prefix("Run a task", "Commands"), "Run a task");
    }

    #[test]
    fn strip_tag_prefix_returns_empty_for_bare_source() {
        assert_eq!(strip_tag_prefix("package.json", "package.json"), "");
    }
}
