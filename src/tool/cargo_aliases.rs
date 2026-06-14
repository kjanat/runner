//! Cargo `[alias]` table — surfaces user-defined and built-in aliases from
//! the hierarchical `.cargo/config.toml` chain as runnable tasks.
//!
//! Cargo probes config files from the current directory up to the filesystem
//! root, then `$CARGO_HOME/config.toml`, merging tables with deeper directories
//! taking precedence. Built-in aliases (`b/c/d/t/r/rm`) cannot be redefined,
//! so we always overwrite user attempts with cargo's defaults — same effective
//! behavior as cargo itself.
//!
//! Each surfaced task carries the *fully-expanded* command string as its alias
//! target: chains like `recursive_example = "rr --example recursions"` resolve
//! through `rr = "run --release"` to `"run --release --example recursions"` so
//! `runner list` shows what cargo will actually execute.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Context as _;
use serde::Deserialize;

const BUILTINS: &[(&str, &str)] = &[
    ("b", "build"),
    ("c", "check"),
    ("d", "doc"),
    ("t", "test"),
    ("r", "run"),
    ("rm", "remove"),
];

/// Maximum recursion depth when expanding alias chains. Cargo errors on
/// genuine cycles; this guard exists so a malicious or pathological config
/// can't make us spin.
const MAX_EXPANSION_DEPTH: usize = 32;

/// One alias as surfaced to the rest of the codebase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExtractedAlias {
    /// The alias name (the cargo subcommand a user types after `cargo `).
    pub name: String,
    /// Fully recursion-expanded token list, suitable for display as
    /// `cargo {tokens...}`. Empty array forms (which cargo would reject)
    /// are filtered out before this point.
    pub expansion: Vec<String>,
}

impl ExtractedAlias {
    /// Render the expansion as a shell-quoted string for alias targets, so
    /// whitespace-bearing tokens round-trip with `tokenize`.
    pub(crate) fn display_command(&self) -> String {
        shlex::try_join(self.expansion.iter().map(String::as_str))
            .unwrap_or_else(|_| self.expansion.join(" "))
    }
}

/// Discover every `.cargo/config{,.toml}` that applies to `start`, in the
/// order cargo would consult them: deepest directory first, then ancestors,
/// then `$CARGO_HOME/config{,.toml}`.
///
/// When both extensioned and non-extensioned forms exist in the same
/// `.cargo/` directory, cargo prefers the file *without* the extension; we
/// mirror that.
pub(crate) fn find_configs(start: &Path) -> Vec<PathBuf> {
    let mut configs = Vec::new();

    for ancestor in start.ancestors() {
        if let Some(path) = pick_config_in(&ancestor.join(".cargo")) {
            configs.push(path);
        }
    }

    if let Some(home) = cargo_home()
        && let Some(path) = pick_config_in(&home)
    {
        // Don't double-list when CARGO_HOME happens to live inside the
        // ancestor walk (e.g. a project under `~`).
        if !configs.iter().any(|existing| existing == &path) {
            configs.push(path);
        }
    }

    configs
}

/// Picks `<dir>/config` if present, else `<dir>/config.toml`, else nothing.
/// Mirrors cargo's "no-extension wins when both exist" rule.
fn pick_config_in(dir: &Path) -> Option<PathBuf> {
    let no_ext = dir.join("config");
    if no_ext.is_file() {
        return Some(no_ext);
    }
    let with_ext = dir.join("config.toml");
    with_ext.is_file().then_some(with_ext)
}

/// Resolve `$CARGO_HOME`, falling back to the platform default
/// (`~/.cargo`) when unset.
fn cargo_home() -> Option<PathBuf> {
    if let Ok(value) = std::env::var("CARGO_HOME")
        && !value.is_empty()
    {
        return Some(PathBuf::from(value));
    }
    home_dir().map(|home| home.join(".cargo"))
}

/// Best-effort home-directory resolution. We deliberately avoid the `home`
/// or `dirs` crates since the values consulted here only feed display +
/// dispatch — never security-sensitive paths.
fn home_dir() -> Option<PathBuf> {
    #[cfg(unix)]
    let var = "HOME";
    #[cfg(windows)]
    let var = "USERPROFILE";

    std::env::var_os(var)
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
}

/// Extract merged + recursion-expanded aliases starting at `dir`.
///
/// Returns built-ins on top of user aliases, with the cargo merge rules
/// applied (deeper > shallower > home > built-ins-can't-be-redefined).
pub(crate) fn extract_tasks(dir: &Path) -> anyhow::Result<Vec<ExtractedAlias>> {
    let configs = find_configs(dir);
    let raw = merge_alias_tables(&configs)?;
    Ok(expand_all(&raw))
}

/// Pick one path to represent the cargo-aliases source for `root` — the
/// deepest applicable `.cargo/config{,.toml}` if one exists, otherwise
/// `<root>/Cargo.toml` so built-ins-only projects still anchor at a real
/// file. Used by `runner list` for the OSC8 link target and by `runner
/// run`'s nearest-source ranking.
pub(crate) fn find_anchor(root: &Path) -> Option<PathBuf> {
    find_configs(root).into_iter().next().or_else(|| {
        let cargo_toml = root.join("Cargo.toml");
        cargo_toml.is_file().then_some(cargo_toml)
    })
}

/// Cargo emits `cargo <name> <user-args...>`; recursion expansion is cargo's
/// own concern at execution time, so we just shell out to the literal name.
pub(crate) fn run_cmd(task: &str, args: &[String]) -> Command {
    let mut c = super::program::command("cargo");
    c.arg(task).args(args);
    c
}

/// Read every config in `paths` (in cargo's discovery order: deepest first)
/// and produce a single alias table where deeper entries win, then overlay
/// built-ins last so they always trump user redefinitions (cargo's rule).
fn merge_alias_tables(paths: &[PathBuf]) -> anyhow::Result<HashMap<String, Vec<String>>> {
    let mut merged: HashMap<String, Vec<String>> = HashMap::new();

    // Walk shallow → deep so deeper writes overwrite shallower ones.
    for path in paths.iter().rev() {
        let aliases =
            read_alias_table(path).with_context(|| format!("reading {}", path.display()))?;
        for (name, value) in aliases {
            let Some(tokens) = tokenize(&value) else {
                anyhow::bail!(
                    "cargo alias `{name}` in {} is unparseable or empty",
                    path.display()
                );
            };
            merged.insert(name, tokens);
        }
    }

    for (name, expansion) in BUILTINS {
        merged.insert((*name).to_string(), vec![(*expansion).to_string()]);
    }

    // Promote each built-in alias's target subcommand to a first-class
    // task (`test`, `build`, …) so the short forms (`t`, `b`) fold under
    // it as aliases instead of standing alone. `entry` keeps any
    // user-defined alias of the same name intact.
    for (_, canonical) in BUILTINS {
        merged
            .entry((*canonical).to_string())
            .or_insert_with(|| vec![(*canonical).to_string()]);
    }

    Ok(merged)
}

#[derive(Deserialize)]
struct ConfigDoc {
    #[serde(default)]
    alias: HashMap<String, AliasValue>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum AliasValue {
    Str(String),
    Arr(Vec<String>),
}

fn read_alias_table(path: &Path) -> anyhow::Result<HashMap<String, AliasValue>> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let doc: ConfigDoc =
        toml::from_str(&content).with_context(|| format!("failed to parse {}", path.display()))?;
    Ok(doc.alias)
}

/// Convert one alias value into its token list. String form gets shell-style
/// splitting via `shlex` so `["command list"]` survives quoting; array form
/// passes through verbatim.
fn tokenize(value: &AliasValue) -> Option<Vec<String>> {
    match value {
        AliasValue::Arr(tokens) => (!tokens.is_empty()).then(|| tokens.clone()),
        AliasValue::Str(raw) => {
            let split = shlex::split(raw)?;
            (!split.is_empty()).then_some(split)
        }
    }
}

/// Expand every alias's head token recursively until it bottoms out at a
/// non-alias or hits the cycle/depth guard. Returns sorted by name.
fn expand_all(map: &HashMap<String, Vec<String>>) -> Vec<ExtractedAlias> {
    let mut out: Vec<ExtractedAlias> = map
        .iter()
        .map(|(name, tokens)| ExtractedAlias {
            name: name.clone(),
            expansion: expand_chain(tokens.clone(), map),
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

fn expand_chain(mut tokens: Vec<String>, map: &HashMap<String, Vec<String>>) -> Vec<String> {
    let mut visited: HashSet<String> = HashSet::new();
    for _ in 0..MAX_EXPANSION_DEPTH {
        let Some(head) = tokens.first().cloned() else {
            return tokens;
        };
        let Some(expansion) = map.get(&head) else {
            return tokens;
        };
        // Canonical subcommands are self-entries (`run → [run]`), so the
        // cycle guard — not the Some() check — terminates them: the head
        // is revisited on the next pass and we bail with the expansion so
        // far. Genuine alias cycles bottom out the same way.
        if !visited.insert(head) {
            return tokens;
        }
        let mut next = expansion.clone();
        next.extend(tokens.drain(1..));
        tokens = next;
    }
    tokens
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{
        AliasValue, BUILTINS, ExtractedAlias, expand_chain, extract_tasks, find_configs,
        merge_alias_tables, pick_config_in, tokenize,
    };
    use crate::tool::test_support::TempDir;

    #[test]
    fn pick_config_prefers_no_extension_when_both_present() {
        let dir = TempDir::new("cargo-aliases-pick");
        fs::create_dir_all(dir.path().join(".cargo")).unwrap();
        fs::write(
            dir.path().join(".cargo").join("config"),
            "[alias]\nx = \"build\"\n",
        )
        .unwrap();
        fs::write(
            dir.path().join(".cargo").join("config.toml"),
            "[alias]\ny = \"build\"\n",
        )
        .unwrap();

        let picked = pick_config_in(&dir.path().join(".cargo")).expect("config should resolve");

        assert!(picked.ends_with("config"));
        assert!(!picked.to_string_lossy().ends_with(".toml"));
    }

    #[test]
    fn find_configs_walks_up_from_nested_dir() {
        let dir = TempDir::new("cargo-aliases-walk");
        let nested = dir.path().join("a").join("b");
        fs::create_dir_all(&nested).unwrap();
        fs::create_dir_all(dir.path().join(".cargo")).unwrap();
        fs::write(dir.path().join(".cargo").join("config.toml"), "").unwrap();
        fs::create_dir_all(nested.join(".cargo")).unwrap();
        fs::write(nested.join(".cargo").join("config.toml"), "").unwrap();

        let configs = find_configs(&nested);
        let names: Vec<_> = configs
            .iter()
            .map(|p| p.parent().unwrap().parent().unwrap().to_path_buf())
            .collect();

        // Deepest first — nested dir's `.cargo` should appear before the
        // ancestor's.
        assert!(names[0].ends_with("b"));
        assert!(names.iter().any(|p| p == dir.path()));
    }

    #[test]
    fn tokenize_string_form_handles_quoted_args() {
        let tokens = tokenize(&AliasValue::Str("run -- \"a b\"".into())).unwrap();
        assert_eq!(tokens, ["run", "--", "a b"]);
    }

    #[test]
    fn tokenize_array_form_passes_through_verbatim() {
        let tokens = tokenize(&AliasValue::Arr(vec!["run".into(), "--release".into()])).unwrap();
        assert_eq!(tokens, ["run", "--release"]);
    }

    #[test]
    fn tokenize_rejects_empty_values() {
        assert!(tokenize(&AliasValue::Str(String::new())).is_none());
        assert!(tokenize(&AliasValue::Arr(Vec::new())).is_none());
    }

    #[test]
    fn merge_overlays_builtins_over_user_redefinitions() {
        let dir = TempDir::new("cargo-aliases-builtin-override");
        fs::create_dir_all(dir.path().join(".cargo")).unwrap();
        fs::write(
            dir.path().join(".cargo").join("config.toml"),
            "[alias]\nb = \"check\"\n",
        )
        .unwrap();

        let merged = merge_alias_tables(&[dir.path().join(".cargo").join("config.toml")]).unwrap();

        assert_eq!(merged.get("b").unwrap(), &vec!["build".to_string()]);
    }

    #[test]
    fn merge_deeper_config_wins_over_ancestor() {
        let dir = TempDir::new("cargo-aliases-deep-wins");
        let nested = dir.path().join("crate");
        fs::create_dir_all(nested.join(".cargo")).unwrap();
        fs::create_dir_all(dir.path().join(".cargo")).unwrap();
        fs::write(
            dir.path().join(".cargo").join("config.toml"),
            "[alias]\nl = \"clippy\"\n",
        )
        .unwrap();
        fs::write(
            nested.join(".cargo").join("config.toml"),
            "[alias]\nl = \"clippy --all-targets\"\n",
        )
        .unwrap();

        let configs = vec![
            nested.join(".cargo").join("config.toml"),
            dir.path().join(".cargo").join("config.toml"),
        ];
        let merged = merge_alias_tables(&configs).unwrap();

        assert_eq!(merged.get("l").unwrap(), &vec!["clippy", "--all-targets"]);
    }

    #[test]
    fn expand_chain_resolves_recursive_aliases() {
        let mut map = std::collections::HashMap::new();
        map.insert("rr".into(), vec!["run".into(), "--release".into()]);
        map.insert(
            "recursive_example".into(),
            vec!["rr".into(), "--example".into(), "recursions".into()],
        );

        let expanded = expand_chain(map["recursive_example"].clone(), &map);

        assert_eq!(expanded, ["run", "--release", "--example", "recursions"]);
    }

    #[test]
    fn expand_chain_breaks_self_referential_cycles() {
        let mut map = std::collections::HashMap::new();
        map.insert("loop".into(), vec!["loop".into(), "--flag".into()]);

        let expanded = expand_chain(map["loop"].clone(), &map);

        // First substitution swaps `loop` for itself; the cycle guard kicks
        // in before infinite append. We tolerate whatever finite stop state
        // it reaches as long as it doesn't loop.
        assert!(expanded.iter().any(|t| t == "--flag"));
    }

    #[test]
    fn extract_tasks_surfaces_builtins_even_without_user_aliases() {
        let dir = TempDir::new("cargo-aliases-builtins-only");
        fs::create_dir_all(dir.path().join(".cargo")).unwrap();
        fs::write(dir.path().join(".cargo").join("config.toml"), "").unwrap();

        let tasks = extract_tasks(dir.path()).unwrap();

        for (name, expansion) in BUILTINS {
            let task = tasks
                .iter()
                .find(|t| t.name == *name)
                .unwrap_or_else(|| panic!("built-in {name} should be surfaced"));
            assert_eq!(task.expansion, vec![(*expansion).to_string()]);
        }
    }

    #[test]
    fn display_command_renders_tokens_space_separated() {
        let alias = ExtractedAlias {
            name: "l".into(),
            expansion: vec![
                "clippy".into(),
                "--all-targets".into(),
                "-D".into(),
                "warnings".into(),
            ],
        };

        assert_eq!(alias.display_command(), "clippy --all-targets -D warnings");
    }

    #[test]
    fn display_command_round_trips_whitespace_tokens() {
        let alias = ExtractedAlias {
            name: "x".into(),
            expansion: vec!["run".into(), "--".into(), "a b".into()],
        };

        let rendered = alias.display_command();
        let reparsed = tokenize(&AliasValue::Str(rendered.clone())).unwrap();
        assert_eq!(reparsed, alias.expansion, "rendered: {rendered}");
    }
}
