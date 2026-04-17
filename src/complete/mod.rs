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

    // `--flag=value` — the current token carries both, we're completing `value`.
    if let Some((flag, _value)) = current.split_once('=')
        && let Some(long) = flag.strip_prefix("--")
        && let Some(hint) = find_long_value_hint(cmd, long)
    {
        return zsh_files_flags(hint);
    }

    // Previous token is `--flag` (no `=`), current token is its value.
    if index > 0 {
        let prev = args[index - 1].to_string_lossy();
        if let Some(long) = prev.strip_prefix("--")
            && !long.contains('=')
            && let Some(hint) = find_long_value_hint(cmd, long)
        {
            return zsh_files_flags(hint);
        }
    }

    None
}

/// Walk `cmd` and its (possibly nested) subcommands to find a long arg
/// named `name`, returning its [`ValueHint`] if any.
fn find_long_value_hint(cmd: &clap::Command, name: &str) -> Option<ValueHint> {
    for arg in cmd.get_arguments() {
        if arg.get_long() == Some(name) {
            return Some(arg.get_value_hint());
        }
    }
    for sub in cmd.get_subcommands() {
        if let Some(hint) = find_long_value_hint(sub, name) {
            return Some(hint);
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

    #[test]
    fn detect_path_files_walks_subcommands() {
        let cmd = Command::new("runner")
            .subcommand(Command::new("run").arg(
                Arg::new("target")
                    .long("target")
                    .value_hint(ValueHint::FilePath)
                    .num_args(1),
            ));
        let args = to_os(&["runner", "run", "--target", ""]);

        assert_eq!(detect_path_files_flags(&cmd, &args, 3), Some(""));
    }

    #[test]
    fn detect_path_files_ignores_non_path_flags() {
        let cmd = Command::new("runner")
            .arg(Arg::new("name").long("name").num_args(1));
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
