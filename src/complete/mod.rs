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

#[cfg(test)]
mod tests {
    use super::strip_tag_prefix;

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
