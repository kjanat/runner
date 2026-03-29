//! Custom shell completion adapters.
//!
//! Provides a zsh adapter that groups completions by
//! [`CompletionCandidate::tag`] so the shell renders section headers
//! (e.g. `-- justfile --`, `-- Commands --`).

use std::ffi::OsString;

use clap_complete::env::{Bash, Elvish, EnvCompleter, Fish, Powershell, Shells};

/// Shell completers with tag-grouped zsh output.
pub(crate) const SHELLS: Shells<'static> =
    Shells(&[&Bash, &Elvish, &Fish, &Powershell, &GroupedZsh]);

/// Tag-aware zsh adapter.
///
/// Emits `TAG\x1fvalue:description` lines from [`write_complete`] and
/// generates a registration script that groups completions under separate
/// `_describe` calls per tag — producing `-- tag --` section headers.
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
        let completions = clap_complete::engine::complete(cmd, args, index, current_dir)?;

        for (i, candidate) in completions.iter().enumerate() {
            if i != 0 {
                write!(buf, "{}", ifs.as_deref().unwrap_or("\n"))?;
            }
            let tag = candidate
                .get_tag()
                .map_or_else(|| "values".to_string(), ToString::to_string);
            write!(
                buf,
                "{}\x1f{}",
                tag,
                escape_value(&candidate.get_value().to_string_lossy()),
            )?;
            if let Some(help) = candidate.get_help() {
                let raw = help.to_string();
                let line = raw.lines().next().unwrap_or_default();
                let stripped = strip_tag_prefix(line, &tag);
                if !stripped.is_empty() {
                    write!(buf, ":{}", escape_help(stripped))?;
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

fn escape_value(string: &str) -> String {
    string.replace('\\', "\\\\").replace(':', "\\:")
}

fn escape_help(string: &str) -> String {
    string.replace('\\', "\\\\")
}

#[cfg(test)]
mod tests {
    use super::{escape_help, escape_value, strip_tag_prefix};

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

    #[test]
    fn escape_value_escapes_colons_and_backslashes() {
        assert_eq!(escape_value("helix:sync"), "helix\\:sync");
        assert_eq!(escape_value("path\\thing"), "path\\\\thing");
    }

    #[test]
    fn escape_help_escapes_backslashes_only() {
        assert_eq!(
            escape_help("justfile: format \\ lint"),
            "justfile: format \\\\ lint"
        );
        assert_eq!(escape_help("no:escaping:here"), "no:escaping:here");
    }
}
