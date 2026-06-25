//! Chain parser. Converts the positional list from CLI into a typed
//! `Vec<ChainItem>` and applies v1 validation rules.

use anyhow::{Result, anyhow, bail};

use super::ChainItem;

/// Parse a positional list of task names into a v1 chain.
///
/// v1 rules (reserved space for v2 quoted bundles — see spec §10):
/// - Positionals containing whitespace are rejected.
/// - Positionals starting with `-` are rejected.
/// - At least one task is required.
pub(crate) fn parse_task_list(raw: &[String]) -> Result<Vec<ChainItem>> {
    if raw.is_empty() {
        bail!("chain mode requires at least one task name");
    }
    let mut out = Vec::with_capacity(raw.len());
    for token in raw {
        validate_v1_token(token)?;
        out.push(ChainItem::task(token));
    }
    Ok(out)
}

fn validate_v1_token(token: &str) -> Result<()> {
    if token.is_empty() {
        bail!("empty task name in chain");
    }
    if token.chars().any(char::is_whitespace) {
        return Err(anyhow!(
            "per-task arguments are not supported in this version\nnote: positional {token:?} \
             contains whitespace\nnote: quoted-bundle syntax is reserved for a future runner \
             release",
        ));
    }
    if token.starts_with('-') {
        return Err(anyhow!(
            "in chain mode, all positionals must be task names (got {token:?}). To forward \
             arguments to a single task, drop `-s`/`-p` and use the classic `run <task> \
             <args...>` form.",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chain::ChainItemKind;

    #[test]
    fn parses_simple_task_list() {
        let items =
            parse_task_list(&["build".into(), "test".into(), "lint".into()]).expect("parses");
        assert_eq!(items.len(), 3);
        assert!(matches!(&items[0].kind, ChainItemKind::Task(n) if n == "build"));
        assert!(items[0].args.is_empty(), "v1 always empty");
    }

    #[test]
    fn rejects_empty_list() {
        let err = parse_task_list(&[]).expect_err("empty list");
        assert!(format!("{err:#}").contains("at least one"));
    }

    #[test]
    fn rejects_token_with_whitespace() {
        let err = parse_task_list(&["build --release".into()]).expect_err("whitespace token");
        let msg = format!("{err:#}");
        assert!(msg.contains("whitespace"), "msg: {msg}");
        assert!(msg.contains("quoted-bundle"), "msg: {msg}");
    }

    #[test]
    fn rejects_token_starting_with_dash() {
        let err = parse_task_list(&["build".into(), "--release".into()])
            .expect_err("dash-prefixed token");
        let msg = format!("{err:#}");
        assert!(msg.contains("task names"), "msg: {msg}");
    }
}
