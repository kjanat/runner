//! Diagnostics for a `runner.toml` buffer.
//!
//! Runs exactly the checks `runner config validate` runs, TOML parse, unknown
//! keys, the deprecation nudges, and the resolver's field/policy validation,
//! against in-memory text, mapping each finding to an editor range. The
//! validation logic itself is reused verbatim from [`crate::config`] and
//! [`crate::resolver`]; only the range-anchoring is LSP-specific.

use std::path::PathBuf;

use lsp_types::{Diagnostic, DiagnosticSeverity, Range};

use super::text::{LineIndex, find_header_range, find_key_range};
use crate::config::{self, LoadedConfig, RunnerConfig};
use crate::types::DetectionWarning;

/// The `source` field stamped on every diagnostic this server emits.
const SOURCE: &str = "runner";

/// Compute diagnostics for `text`. A fatal parse error short-circuits (the rest
/// of the pipeline needs a parsed document); otherwise every check runs and the
/// findings are merged.
pub(super) fn compute(text: &str, index: &LineIndex) -> Vec<Diagnostic> {
    let value: toml::Value = match toml::from_str(text) {
        Ok(value) => value,
        Err(error) => return vec![parse_error(text, index, &error)],
    };

    let mut out = Vec::new();
    for warning in config::collect_unknown_keys(&value) {
        out.push(warning_diagnostic(text, index, &warning));
    }

    // Deserialize from the text, not the parsed `value`: the text-based
    // deserializer spans a wrong-typed known field, so the diagnostic can
    // point at the offending value instead of line one.
    let config: RunnerConfig = match toml::from_str(text) {
        Ok(config) => config,
        Err(error) => {
            out.push(parse_error(text, index, &error));
            return out;
        }
    };

    let loaded = LoadedConfig {
        path: PathBuf::from("runner.toml"),
        config,
        warnings: Vec::new(),
    };
    if let Err(error) = crate::resolver::validate_config(&loaded) {
        let message = format!("{error:#}");
        let range =
            anchor_from_message(text, index, &message).unwrap_or_else(|| index.line_range(text, 0));
        out.push(error_diagnostic(range, message));
    }

    out
}

/// Map a TOML parse error to a diagnostic, using the parser's own span when it
/// has one and falling back to the first line otherwise.
fn parse_error(text: &str, index: &LineIndex, error: &toml::de::Error) -> Diagnostic {
    let range = error.span().map_or_else(
        || index.line_range(text, 0),
        |span| index.range(text, span.start, span.end),
    );
    error_diagnostic(range, error.message().to_string())
}

/// Build a `WARNING`-severity diagnostic for an unknown key, anchored at the
/// offending key or section.
fn warning_diagnostic(text: &str, index: &LineIndex, warning: &DetectionWarning) -> Diagnostic {
    let path = match warning {
        DetectionWarning::UnknownConfigKey { path } => path.as_str(),
        _ => warning.source(),
    };
    let range = range_for_path(text, index, path).unwrap_or_else(|| index.line_range(text, 0));
    Diagnostic {
        range,
        severity: Some(DiagnosticSeverity::WARNING),
        source: Some(SOURCE.to_string()),
        message: warning.detail(),
        ..Diagnostic::default()
    }
}

/// An `ERROR`-severity diagnostic at `range`.
fn error_diagnostic(range: Range, message: String) -> Diagnostic {
    Diagnostic {
        range,
        severity: Some(DiagnosticSeverity::ERROR),
        source: Some(SOURCE.to_string()),
        message,
        ..Diagnostic::default()
    }
}

/// Resolve a dotted key path to its buffer range: the key under its table's
/// header, else the header itself.
fn range_for_path(text: &str, index: &LineIndex, path: &str) -> Option<Range> {
    match path.rsplit_once('.') {
        Some((section, field)) => find_key_range(index, text, Some(section), field)
            .or_else(|| find_header_range(index, text, path))
            .or_else(|| find_header_range(index, text, section)),
        None => {
            find_header_range(index, text, path).or_else(|| find_key_range(index, text, None, path))
        }
    }
}

/// Anchor for a resolver error, whose message starts with the dotted key it
/// rejects: `runner.toml tasks.build.pm: …`.
fn anchor_from_message(text: &str, index: &LineIndex, message: &str) -> Option<Range> {
    let rest = message.strip_prefix("runner.toml ").unwrap_or(message);
    let (path, _) = rest.split_once(':')?;
    range_for_path(text, index, path.trim())
}

#[cfg(test)]
mod tests {
    use lsp_types::DiagnosticSeverity;

    use super::{LineIndex, compute};

    fn diagnostics(text: &str) -> Vec<lsp_types::Diagnostic> {
        compute(text, &LineIndex::new(text))
    }

    #[test]
    fn clean_config_is_silent() {
        let text = "download = \"ask\"\n[tasks.build]\npm = \"pnpm\"\n";
        assert!(diagnostics(text).is_empty(), "{:?}", diagnostics(text));
    }

    #[test]
    fn unknown_key_warns() {
        let found = diagnostics("[nope]\nx = 1\n");
        assert!(found.iter().any(|d| {
            d.severity == Some(DiagnosticSeverity::WARNING) && d.message.contains("unknown key")
        }));
    }

    #[test]
    fn a_nested_unknown_key_anchors_to_its_line() {
        let text = "[output.task]\nstdrr = true\n";
        let found = diagnostics(text);
        let diag = found
            .iter()
            .find(|d| d.severity == Some(DiagnosticSeverity::WARNING))
            .expect("a warning");
        assert_eq!(diag.range.start.line, 1, "{diag:?}");
    }

    #[test]
    fn an_unknown_provider_errors_at_its_key() {
        let text = "[tasks.build]\npm = \"zoot\"\n";
        let found = diagnostics(text);
        let diag = found
            .iter()
            .find(|d| d.severity == Some(DiagnosticSeverity::ERROR))
            .expect("an error diagnostic");
        assert!(diag.message.contains("unknown package manager"), "{diag:?}");
        assert_eq!(diag.range.start.line, 1, "{diag:?}");
    }

    #[test]
    fn type_error_anchors_to_the_offending_value() {
        let found = diagnostics("[install]\nfrozen = \"yes\"\n");
        let diag = found
            .iter()
            .find(|d| d.severity == Some(DiagnosticSeverity::ERROR))
            .expect("expected an error diagnostic");
        assert_eq!(diag.range.start.line, 1, "{diag:?}");
    }

    #[test]
    fn syntax_error_is_reported() {
        let found = diagnostics("[install]\nfrozen = \n");
        assert!(
            found
                .iter()
                .any(|d| d.severity == Some(DiagnosticSeverity::ERROR))
        );
    }
}
