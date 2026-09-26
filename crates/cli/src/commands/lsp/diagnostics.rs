//! Diagnostics for a `runner.toml` buffer.
//!
//! Runs exactly the checks `runner config validate` runs, TOML parse, unknown
//! keys, and the resolver's value validation, against in-memory text, mapping
//! each finding to the range of the key it names. The validation logic itself
//! is reused verbatim from [`crate::config`] and [`crate::resolver`]; only the
//! range-anchoring is LSP-specific.

use std::path::PathBuf;

use lsp_types::{Diagnostic, DiagnosticSeverity, Range};

use super::syntax::Document;
use super::text::LineIndex;
use crate::config::{self, KeyPath, LoadedConfig, RunnerConfig};
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
    let document = Document::parse(text);
    let at = |path: &KeyPath| {
        document.span_of(path.keys()).map_or_else(
            || index.line_range(text, 0),
            |span| index.range(text, span.start, span.end),
        )
    };

    let mut out = Vec::new();
    for warning in config::collect_unknown_keys(&value) {
        if let DetectionWarning::UnknownConfigKey { path } = &warning {
            out.push(diagnostic(
                at(path),
                DiagnosticSeverity::WARNING,
                warning.detail(),
            ));
        }
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
        path: PathBuf::from(config::CONFIG_FILENAME),
        config,
        warnings: Vec::new(),
    };
    for issue in crate::resolver::config_issues(&loaded) {
        if let DetectionWarning::InvalidConfigValue { key, message, .. } = &issue {
            out.push(diagnostic(
                at(key),
                DiagnosticSeverity::ERROR,
                format!("{key}: {message}"),
            ));
        }
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
    diagnostic(
        range,
        DiagnosticSeverity::ERROR,
        error.message().to_string(),
    )
}

fn diagnostic(range: Range, severity: DiagnosticSeverity, message: String) -> Diagnostic {
    Diagnostic {
        range,
        severity: Some(severity),
        source: Some(SOURCE.to_string()),
        message,
        ..Diagnostic::default()
    }
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
    fn a_quoted_task_key_anchors_its_error_to_its_line() {
        let text = "[tasks.\"package.json:build\"]\npm = \"zoot\"\n";
        let found = diagnostics(text);
        let diag = found
            .iter()
            .find(|d| d.severity == Some(DiagnosticSeverity::ERROR))
            .expect("an error diagnostic");
        assert!(
            diag.message.starts_with("tasks.\"package.json:build\".pm:"),
            "{diag:?}"
        );
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
