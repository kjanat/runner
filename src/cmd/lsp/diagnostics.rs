//! Diagnostics for a `runner.toml` buffer.
//!
//! Runs exactly the checks `runner config validate` runs, TOML parse, unknown
//! keys, the deprecation nudges, and the resolver's field/policy validation,
//! against in-memory text, mapping each finding to an editor range. The
//! validation logic itself is reused verbatim from [`crate::config`] and
//! [`crate::resolver`]; only the range-anchoring is LSP-specific.

use std::path::PathBuf;

use lsp_types::{Diagnostic, DiagnosticSeverity, DiagnosticTag, Range};

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

    for warning in config::deprecation_warnings(&config) {
        out.push(warning_diagnostic(text, index, &warning));
    }

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

/// Build a `WARNING`-severity diagnostic for an unknown or deprecated key,
/// anchored at the offending key/section (tagged `DEPRECATED` for the latter).
fn warning_diagnostic(text: &str, index: &LineIndex, warning: &DetectionWarning) -> Diagnostic {
    let (path, tags) = match warning {
        DetectionWarning::UnknownConfigKey { path } => (path.as_str(), None),
        DetectionWarning::DeprecatedConfigKey { path, .. } => {
            (path.as_str(), Some(vec![DiagnosticTag::DEPRECATED]))
        }
        // collect_unknown_keys / deprecation_warnings only emit the two above.
        _ => (warning.source(), None),
    };
    let range = range_for_path(text, index, path).unwrap_or_else(|| index.line_range(text, 0));
    Diagnostic {
        range,
        severity: Some(DiagnosticSeverity::WARNING),
        source: Some(SOURCE.to_string()),
        message: warning.detail(),
        tags,
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

/// Resolve a dotted `section[.field]` path to a buffer range.
fn range_for_path(text: &str, index: &LineIndex, path: &str) -> Option<Range> {
    match path.split_once('.') {
        Some((section, field)) => find_key_range(index, text, Some(section), field),
        None => find_header_range(index, text, path),
    }
}

/// Best-effort anchor for a resolver error: most messages begin with a
/// `[section].field` or `[section]` reference, e.g. `[tasks].prefer: …` or
/// `[pm].node: …`. `[tasks.overrides]` entries instead read `[tasks.overrides]
/// "task": …` (the entry key is user-chosen, not a schema field), so that form
/// is checked first. Parse whichever leading reference is present and map it
/// to a range.
fn anchor_from_message(text: &str, index: &LineIndex, message: &str) -> Option<Range> {
    let rest = message.strip_prefix('[')?;
    let (section, rest) = rest.split_once(']')?;
    let section = section.trim();

    if let Some(key) = rest
        .trim_start()
        .strip_prefix('"')
        .and_then(|tail| tail.split_once('"'))
        .map(|(key, _)| key)
        && let Some(range) = find_key_range(index, text, Some(section), key)
    {
        return Some(range);
    }

    let field = rest
        .strip_prefix('.')
        .map(|tail| {
            tail.trim_start()
                .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                .next()
                .unwrap_or("")
        })
        .filter(|f| !f.is_empty());

    field.map_or_else(
        || find_header_range(index, text, section),
        |field| {
            find_key_range(index, text, Some(section), field)
                .or_else(|| find_header_range(index, text, section))
        },
    )
}

#[cfg(test)]
mod tests {
    use lsp_types::{DiagnosticSeverity, DiagnosticTag};

    use super::{LineIndex, compute};

    fn diagnostics(text: &str) -> Vec<lsp_types::Diagnostic> {
        compute(text, &LineIndex::new(text))
    }

    #[test]
    fn clean_config_is_silent() {
        let text = "[tasks]\nprefer = [\"turbo\", \"bun\"]\n";
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
    fn unknown_label_errors() {
        let found = diagnostics("[tasks]\nprefer = [\"zoot\"]\n");
        assert!(found.iter().any(|d| {
            d.severity == Some(DiagnosticSeverity::ERROR) && d.message.contains("unknown source")
        }));
    }

    #[test]
    fn deprecated_key_is_tagged() {
        let found = diagnostics("[task_runner]\nprefer = [\"turbo\"]\n");
        assert!(found.iter().any(|d| {
            d.tags
                .as_ref()
                .is_some_and(|tags| tags.contains(&DiagnosticTag::DEPRECATED))
        }));
    }

    #[test]
    fn type_error_anchors_to_the_offending_value() {
        let found = diagnostics("[install]\npms = \"bun\"\n");
        let diag = found
            .iter()
            .find(|d| d.severity == Some(DiagnosticSeverity::ERROR))
            .expect("expected an error diagnostic");
        assert_eq!(diag.range.start.line, 1, "{diag:?}");
        assert!(diag.message.contains("expected a sequence"), "{diag:?}");
    }

    #[test]
    fn syntax_error_is_reported() {
        let found = diagnostics("[pm]\nnode = \n");
        assert!(
            found
                .iter()
                .any(|d| d.severity == Some(DiagnosticSeverity::ERROR))
        );
    }

    #[test]
    fn tasks_overrides_error_anchors_to_the_offending_entry() {
        let text = "[tasks.overrides]\nbuild = \"zoot\"\n";
        let found = diagnostics(text);
        let diag = found
            .iter()
            .find(|d| d.severity == Some(DiagnosticSeverity::ERROR))
            .expect("expected an error diagnostic");
        assert_eq!(diag.range.start.line, 1, "{diag:?}");
    }
}
