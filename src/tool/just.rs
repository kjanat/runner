//! just — a handy command runner using `justfile`.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Context as _;
use serde::Deserialize;

use crate::tool::files;

pub(crate) const FILENAMES: &[&str] = &["justfile", "Justfile", ".justfile"];

/// A task extracted from a justfile: either a public recipe or an alias.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ExtractedTask {
    Recipe { name: String, doc: Option<String> },
    Alias { name: String, target: String },
}

impl ExtractedTask {
    pub(crate) fn name(&self) -> &str {
        match self {
            Self::Recipe { name, .. } | Self::Alias { name, .. } => name,
        }
    }
}

/// Detected via `justfile`, `Justfile`, or `.justfile`.
pub(crate) fn detect(dir: &Path) -> bool {
    find_file(dir).is_some()
}

/// Parse public recipes and aliases from a justfile.
pub(crate) fn extract_tasks(dir: &Path) -> anyhow::Result<Vec<ExtractedTask>> {
    let Some(path) = find_file(dir) else {
        return Ok(vec![]);
    };

    extract_tasks_with_just(&path).map_or_else(|| extract_tasks_from_source(&path), Ok)
}

fn extract_tasks_with_just(path: &Path) -> Option<Vec<ExtractedTask>> {
    #[derive(Deserialize)]
    struct Dump {
        recipes: HashMap<String, Recipe>,
        #[serde(default)]
        aliases: HashMap<String, Alias>,
        #[serde(default)]
        modules: HashMap<String, Module>,
    }

    #[derive(Deserialize)]
    struct Module {
        #[serde(default)]
        recipes: HashMap<String, Recipe>,
        #[serde(default)]
        modules: HashMap<String, Self>,
    }

    #[derive(Deserialize)]
    struct Recipe {
        private: bool,
        doc: Option<String>,
    }

    #[derive(Deserialize)]
    struct Alias {
        #[serde(default)]
        private: bool,
        target: String,
    }

    fn any_module_has_recipe(modules: &HashMap<String, Module>, name: &str) -> bool {
        let mut stack: Vec<&HashMap<String, Module>> = vec![modules];
        while let Some(current) = stack.pop() {
            for module in current.values() {
                if module.recipes.contains_key(name) {
                    return true;
                }
                stack.push(&module.modules);
            }
        }
        false
    }

    let output = Command::new("just")
        .arg("--justfile")
        .arg(path)
        .arg("--dump-format")
        .arg("json")
        .arg("--dump")
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let dump = serde_json::from_slice::<Dump>(&output.stdout).ok()?;
    let mut tasks: Vec<ExtractedTask> = dump
        .recipes
        .iter()
        .filter(|(_, recipe)| !recipe.private)
        .map(|(name, recipe)| ExtractedTask::Recipe {
            name: name.clone(),
            doc: recipe.doc.clone(),
        })
        .collect();
    for (name, alias) in &dump.aliases {
        if alias.private
            || name.starts_with('_')
            || alias_target_leaf(&alias.target).starts_with('_')
        {
            continue;
        }
        // `just --dump` normalizes submodule alias targets to the leaf name
        // (e.g. `alias b := foo::bar` becomes `target: "bar"`), so a top-level
        // recipe of the same name is indistinguishable from a submodule one.
        // When both exist, treat the alias as unresolved to avoid attributing
        // the wrong recipe's doc/privacy to it.
        let ambiguous = any_module_has_recipe(&dump.modules, &alias.target);
        match dump.recipes.get(&alias.target) {
            Some(target) if !ambiguous && target.private => {}
            _ => tasks.push(ExtractedTask::Alias {
                name: name.clone(),
                target: alias.target.clone(),
            }),
        }
    }
    tasks.sort_unstable_by(|a, b| a.name().cmp(b.name()));
    Some(tasks)
}

/// Resolve the active justfile path in the current directory.
///
/// Honors standard filenames and falls back to an ASCII case-insensitive
/// `justfile` match (e.g. `JUSTFILE`).
pub(crate) fn find_file(dir: &Path) -> Option<PathBuf> {
    if let Some(path) = files::find_first(dir, FILENAMES).filter(|path| path.is_file()) {
        return Some(path);
    }

    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.eq_ignore_ascii_case("justfile"))
        })
        .collect();

    paths.sort_unstable();
    paths.into_iter().next()
}

struct ParsedRecipe {
    doc: Option<String>,
    private: bool,
}

struct ParsedAlias {
    name: String,
    target: String,
    private: bool,
}

fn is_top_level_directive(trimmed: &str) -> bool {
    ["set ", "import ", "include ", "mod ", "export "]
        .iter()
        .any(|prefix| trimmed.starts_with(prefix))
}

fn try_upsert_recipe(
    recipes: &mut HashMap<String, ParsedRecipe>,
    trimmed: &str,
    saw_private_attr: bool,
    last_doc: Option<String>,
) {
    let recipe = trimmed.strip_prefix('@').unwrap_or(trimmed);
    let Some(colon) = recipe.find(':') else {
        return;
    };
    // `foo := "bar"` is a variable binding, not a recipe header.
    if recipe[colon..].starts_with(":=") {
        return;
    }
    let name = recipe[..colon].split_whitespace().next().unwrap_or("");
    if !is_valid_ident(name) {
        return;
    }
    let private = saw_private_attr || name.starts_with('_');
    let doc = last_doc.filter(|d| !d.is_empty());
    match recipes.entry(name.to_string()) {
        Entry::Vacant(slot) => {
            slot.insert(ParsedRecipe { doc, private });
        }
        Entry::Occupied(mut slot) => {
            // A later `[private]` annotation on the same recipe name must
            // promote the aggregate to private; losing the flag would surface
            // a recipe the author hid.
            let existing = slot.get_mut();
            existing.private |= private;
            if existing.doc.is_none() {
                existing.doc = doc;
            }
        }
    }
}

fn extract_tasks_from_source(path: &Path) -> anyhow::Result<Vec<ExtractedTask>> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let mut recipes: HashMap<String, ParsedRecipe> = HashMap::new();
    let mut aliases: Vec<ParsedAlias> = Vec::new();
    let mut saw_private_attr = false;
    let mut last_doc: Option<String> = None;
    for line in content.lines() {
        if line.starts_with(' ') || line.starts_with('\t') {
            continue;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            last_doc = None;
            continue;
        }
        if let Some(comment) = trimmed.strip_prefix('#') {
            last_doc = Some(comment.trim().to_string());
            continue;
        }
        if trimmed.starts_with('[') {
            saw_private_attr |= is_private_attr(trimmed);
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("alias ") {
            if let Some((name, target)) = parse_alias(rest) {
                let private = saw_private_attr || name.starts_with('_');
                aliases.push(ParsedAlias {
                    name,
                    target,
                    private,
                });
            }
        } else if !is_top_level_directive(trimmed) {
            try_upsert_recipe(&mut recipes, trimmed, saw_private_attr, last_doc.take());
        }
        saw_private_attr = false;
        last_doc = None;
    }

    let mut tasks: Vec<ExtractedTask> = recipes
        .iter()
        .filter(|(_, r)| !r.private)
        .map(|(name, r)| ExtractedTask::Recipe {
            name: name.clone(),
            doc: r.doc.clone(),
        })
        .collect();
    for alias in aliases {
        if alias.private || alias_target_leaf(&alias.target).starts_with('_') {
            continue;
        }
        match recipes.get(&alias.target) {
            Some(target) if target.private => {}
            _ => tasks.push(ExtractedTask::Alias {
                name: alias.name,
                target: alias.target,
            }),
        }
    }
    tasks.sort_unstable_by(|a, b| a.name().cmp(b.name()));
    Ok(tasks)
}

fn parse_alias(rest: &str) -> Option<(String, String)> {
    let (name_part, target_part) = rest.split_once(":=")?;
    let name = name_part.trim();
    let target = target_part.split_whitespace().next().unwrap_or("");
    if !is_valid_ident(name) {
        return None;
    }
    if !target.split("::").all(is_valid_ident) {
        return None;
    }
    Some((name.to_string(), target.to_string()))
}

fn alias_target_leaf(target: &str) -> &str {
    target.rsplit_once("::").map_or(target, |(_, leaf)| leaf)
}

fn is_valid_ident(s: &str) -> bool {
    let mut chars = s.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn is_private_attr(trimmed: &str) -> bool {
    trimmed
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .is_some_and(|attr| {
            attr.split(',')
                .map(str::trim)
                .any(|segment| segment.starts_with("private"))
        })
}

/// `just <task> [args...]`
pub(crate) fn run_cmd(task: &str, args: &[String]) -> Command {
    let mut c = Command::new("just");
    c.arg(task).args(args);
    c
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::process::Command;

    use super::{
        ExtractedTask, detect, extract_tasks, extract_tasks_from_source, extract_tasks_with_just,
        is_private_attr, parse_alias,
    };
    use crate::tool::test_support::TempDir;

    #[test]
    fn fallback_parser_skips_private_and_directive_lines() {
        let dir = TempDir::new("just-fallback");
        let path = dir.path().join("justfile");

        fs::write(
            &path,
            "set shell := [\"bash\", \"-cu\"]\ninclude \"common.just\"\n[private]\nfoo := \"bar\"\n\n[private]\nsecret:\n  echo nope\n\nbuild:\n  echo build\n\n_secret:\n  echo nope\n\n@quiet name=\"world\":\n  echo hi {{name}}\n",
        )
        .expect("justfile should be written");

        let tasks = extract_tasks_from_source(&path).expect("justfile source should parse");
        let names: Vec<&str> = tasks.iter().map(ExtractedTask::name).collect();
        assert_eq!(names, ["build", "quiet"]);
    }

    #[test]
    fn private_attr_matches_comma_separated_lists() {
        assert!(is_private_attr("[unix, private]"));
        assert!(is_private_attr("[private(no-cd), unix]"));
        assert!(!is_private_attr("[unix, linux]"));
    }

    #[test]
    fn fallback_parser_enforces_just_name_grammar() {
        // Just's grammar is `NAME = [a-zA-Z_][a-zA-Z0-9_-]*`. Names that
        // start with a digit or hyphen, or contain non-ASCII letters, are
        // rejected by `just` itself and must not be surfaced by the
        // fallback parser either.
        let dir = TempDir::new("just-fallback-ident-grammar");
        let path = dir.path().join("justfile");

        fs::write(
            &path,
            "1build:\n  echo nope\n\n-build:\n  echo nope\n\néclair:\n  echo nope\n\nβuild:\n  echo nope\n\nbuild:\n  echo yes\n",
        )
        .expect("justfile should be written");

        let tasks = extract_tasks_from_source(&path).expect("justfile source should parse");
        let names: Vec<&str> = tasks.iter().map(ExtractedTask::name).collect();
        assert_eq!(names, ["build"]);
    }

    #[test]
    fn extract_tasks_uses_just_json_when_available() {
        if Command::new("just").arg("--version").output().is_err() {
            eprintln!("skipping: just unavailable");
            return;
        }

        let dir = TempDir::new("just-json");
        fs::write(
            dir.path().join("justfile"),
            "build:\n  echo build\n\n_secret:\n  echo nope\n\n@quiet:\n  echo hi\n",
        )
        .expect("justfile should be written");

        let tasks = extract_tasks(dir.path()).expect("justfile tasks should parse");
        let names: Vec<&str> = tasks.iter().map(ExtractedTask::name).collect();
        assert_eq!(names, ["build", "quiet"]);
    }

    #[test]
    fn detect_supports_uppercase_justfile_name() {
        let dir = TempDir::new("just-uppercase");
        fs::write(dir.path().join("JUSTFILE"), "build:\n  echo build\n")
            .expect("JUSTFILE should be written");

        assert!(detect(dir.path()));
    }

    #[test]
    fn parse_alias_accepts_standard_forms() {
        assert_eq!(
            parse_alias("b := build"),
            Some(("b".to_string(), "build".to_string()))
        );
        assert_eq!(
            parse_alias("b:=build"),
            Some(("b".to_string(), "build".to_string()))
        );
        assert_eq!(
            parse_alias("b := build # trailing"),
            Some(("b".to_string(), "build".to_string()))
        );
        assert_eq!(parse_alias("b build"), None);
        assert_eq!(parse_alias("b := "), None);
    }

    #[test]
    fn parse_alias_accepts_submodule_target() {
        assert_eq!(
            parse_alias("b := foo::bar"),
            Some(("b".to_string(), "foo::bar".to_string()))
        );
        assert_eq!(
            parse_alias("q := a::b::c"),
            Some(("q".to_string(), "a::b::c".to_string()))
        );
        assert_eq!(parse_alias("b := foo::"), None);
        assert_eq!(parse_alias("b := ::bar"), None);
    }

    #[test]
    fn fallback_parser_emits_submodule_aliases_without_doc() {
        let dir = TempDir::new("just-alias-submodule");
        let path = dir.path().join("justfile");

        fs::write(&path, "mod foo\n\nalias b := foo::bar\n").expect("justfile should be written");

        let tasks = extract_tasks_from_source(&path).expect("justfile source should parse");
        assert_eq!(
            tasks,
            vec![ExtractedTask::Alias {
                name: "b".to_string(),
                target: "foo::bar".to_string(),
            }]
        );
    }

    #[test]
    fn fallback_parser_extracts_public_aliases() {
        let dir = TempDir::new("just-alias-public");
        let path = dir.path().join("justfile");

        fs::write(
            &path,
            "# Build the project\nbuild:\n  echo build\n\nalias b := build\n",
        )
        .expect("justfile should be written");

        let tasks = extract_tasks_from_source(&path).expect("justfile source should parse");
        assert_eq!(
            tasks,
            vec![
                ExtractedTask::Alias {
                    name: "b".to_string(),
                    target: "build".to_string(),
                },
                ExtractedTask::Recipe {
                    name: "build".to_string(),
                    doc: Some("Build the project".to_string()),
                },
            ]
        );
    }

    #[test]
    fn fallback_parser_hides_aliases_to_private_recipes() {
        let dir = TempDir::new("just-alias-private-target");
        let path = dir.path().join("justfile");

        fs::write(
            &path,
            "_secret:\n  echo nope\n\n[private]\nhush:\n  echo nope\n\nalias s := _secret\nalias h := hush\n",
        )
        .expect("justfile should be written");

        let tasks = extract_tasks_from_source(&path).expect("justfile source should parse");
        let names: Vec<&str> = tasks.iter().map(ExtractedTask::name).collect();
        assert!(names.is_empty(), "expected no tasks, got {names:?}");
    }

    #[test]
    fn fallback_parser_hides_private_aliases() {
        let dir = TempDir::new("just-alias-private-alias");
        let path = dir.path().join("justfile");

        fs::write(
            &path,
            "build:\n  echo build\n\nalias _hidden := build\n[private]\nalias h := build\n",
        )
        .expect("justfile should be written");

        let tasks = extract_tasks_from_source(&path).expect("justfile source should parse");
        let names: Vec<&str> = tasks.iter().map(ExtractedTask::name).collect();
        assert_eq!(names, ["build"]);
    }

    #[test]
    fn extract_tasks_uses_just_json_when_available_with_aliases() {
        let dir = TempDir::new("just-json-aliases");
        let path = dir.path().join("justfile");
        fs::write(
            &path,
            "# Build the project\nbuild:\n  echo build\n\n_secret:\n  echo nope\n\nalias b := build\nalias s := _secret\nalias _hidden := build\n",
        )
        .expect("justfile should be written");

        let Some(tasks) = extract_tasks_with_just(&path) else {
            eprintln!("skipping: just unavailable");
            return;
        };
        let names: Vec<&str> = tasks.iter().map(ExtractedTask::name).collect();
        assert_eq!(names, ["b", "build"]);
        let b = tasks
            .iter()
            .find(|t| t.name() == "b")
            .expect("alias b should surface");
        assert!(
            matches!(b, ExtractedTask::Alias { target, .. } if target == "build"),
            "expected alias b → build, got {b:?}"
        );
    }

    #[test]
    fn fallback_parser_hides_aliases_to_private_submodule_targets() {
        let dir = TempDir::new("just-alias-submodule-private");
        let path = dir.path().join("justfile");

        fs::write(
            &path,
            "mod foo\n\nbuild:\n  echo build\n\nalias s := foo::_secret\nalias b := build\n",
        )
        .expect("justfile should be written");

        let tasks = extract_tasks_from_source(&path).expect("justfile source should parse");
        let names: Vec<&str> = tasks.iter().map(ExtractedTask::name).collect();
        assert_eq!(names, ["b", "build"]);
    }

    #[test]
    fn json_alias_targeting_submodule_recipe_is_unresolved() {
        let dir = TempDir::new("just-json-alias-ambig");
        let root = dir.path();
        fs::create_dir_all(root.join("foo")).expect("foo dir");
        fs::write(
            root.join("foo/mod.just"),
            "# submodule bar\nbar:\n  echo sub\n",
        )
        .expect("module justfile should be written");
        let path = root.join("justfile");
        fs::write(
            &path,
            "mod foo\n\n# top bar\nbar:\n  echo top\n\nalias b := foo::bar\n",
        )
        .expect("justfile should be written");

        let Some(tasks) = extract_tasks_with_just(&path) else {
            eprintln!("skipping: just unavailable");
            return;
        };
        let b = tasks
            .iter()
            .find(|t| t.name() == "b")
            .expect("alias b should be surfaced");
        assert!(
            matches!(b, ExtractedTask::Alias { target, .. } if target == "bar"),
            "ambiguous submodule alias must surface as Alias with leaf target, got {b:?}"
        );
    }
}
