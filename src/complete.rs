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

        // The script collects `TAG\x1fvalue:desc` lines, extracts unique tags
        // in insertion order, then calls `_describe TAG entries` per group.
        let script = r#"#compdef BIN
function _clap_dynamic_completer_NAME() {
    local _CLAP_COMPLETE_INDEX=$(expr $CURRENT - 1)
    local _CLAP_IFS=$'\n'

    local raw=("${(@f)$( \
        _CLAP_IFS="$_CLAP_IFS" \
        _CLAP_COMPLETE_INDEX="$_CLAP_COMPLETE_INDEX" \
        VAR="zsh" \
        COMPLETER -- "${words[@]}" 2>/dev/null \
    )}")

    [[ -z "$raw" ]] && return

    local -a _tags=()
    local _line
    for _line in "${raw[@]}"; do
        local _tag="${_line%%$'\x1f'*}"
        if (( ! ${_tags[(Ie)$_tag]} )); then
            _tags+=("$_tag")
        fi
    done

    local _tag
    for _tag in "${_tags[@]}"; do
        local -a _entries=()
        for _line in "${raw[@]}"; do
            if [[ "${_line%%$'\x1f'*}" == "$_tag" ]]; then
                _entries+=("${_line#*$'\x1f'}")
            fi
        done
        (( ${#_entries} )) && _describe "$_tag" _entries
    done
}

compdef _clap_dynamic_completer_NAME BIN"#
            .replace("NAME", &escaped_name)
            .replace("COMPLETER", &completer)
            .replace("BIN", &bin)
            .replace("VAR", var);

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
                write!(
                    buf,
                    ":{}",
                    escape_help(help.to_string().lines().next().unwrap_or_default()),
                )?;
            }
        }
        Ok(())
    }
}

fn escape_value(string: &str) -> String {
    string.replace('\\', "\\\\").replace(':', "\\:")
}

fn escape_help(string: &str) -> String {
    string.replace('\\', "\\\\")
}
