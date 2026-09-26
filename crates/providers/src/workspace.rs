//! Workspace member expansion shared by every declaring provider.

use std::path::{Path, PathBuf};

use runner_core::Warning;

/// What a member directory's manifest says about its name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Manifest {
    /// The directory carries no manifest, so it is no member.
    Absent,
    /// The manifest declares no name.
    Unnamed,
    /// The manifest's name.
    Named(String),
}

impl Manifest {
    /// A present manifest declaring `name`, if any.
    #[must_use]
    pub fn named(name: Option<&str>) -> Self {
        name.map_or(Self::Unnamed, |name| Self::Named(name.to_owned()))
    }
}

/// A directory's [`Manifest`].
pub type ManifestName = fn(&Path) -> Result<Manifest, Warning>;

/// The directories `globs` (with `!` negations) name under `root` that carry
/// a manifest, each with the name that manifest declares.
///
/// # Errors
///
/// Returns a malformed glob or a manifest that cannot be read.
pub fn members(
    root: &Path,
    globs: &[String],
    manifest: ManifestName,
) -> Result<Vec<(Option<String>, PathBuf)>, Warning> {
    let mut found = Vec::new();
    for dir in expand(root, globs)? {
        match manifest(&dir)? {
            Manifest::Absent => {}
            Manifest::Unnamed => found.push((None, dir)),
            Manifest::Named(name) => found.push((Some(name), dir)),
        }
    }
    Ok(found)
}

/// A file read to text, `None` when it does not exist.
///
/// # Errors
///
/// Returns the path and the failure for any other read error.
pub fn read(path: &Path) -> Result<Option<String>, Warning> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(Warning::general(format!("{}: {error}", path.display()))),
    }
}

const MATCH_OPTIONS: glob::MatchOptions = glob::MatchOptions {
    case_sensitive: true,
    require_literal_separator: true,
    require_literal_leading_dot: true,
};

fn expand(root: &Path, globs: &[String]) -> Result<Vec<PathBuf>, Warning> {
    let (negatives, positives): (Vec<&str>, Vec<&str>) = globs
        .iter()
        .map(String::as_str)
        .partition(|glob| glob.starts_with('!'));
    let negatives: Vec<glob::Pattern> = negatives
        .iter()
        .map(|glob| {
            glob::Pattern::new(&normalize(&glob[1..]))
                .map_err(|error| Warning::general(format!("workspace glob {glob}: {error}")))
        })
        .collect::<Result<_, _>>()?;
    let escaped_root = glob::Pattern::escape(&root.to_string_lossy());
    let mut dirs: Vec<PathBuf> = Vec::new();
    for positive in positives {
        let pattern = normalize(positive);
        if pattern.is_empty() {
            continue;
        }
        let paths = glob::glob_with(&format!("{escaped_root}/{pattern}"), MATCH_OPTIONS)
            .map_err(|error| Warning::general(format!("workspace glob {pattern}: {error}")))?;
        for path in paths {
            let path = path.map_err(|error| Warning::general(error.to_string()))?;
            let Ok(relative) = path.strip_prefix(root) else {
                continue;
            };
            let in_node_modules = relative
                .components()
                .any(|component| component.as_os_str() == "node_modules");
            let excluded = negatives
                .iter()
                .any(|negative| negative.matches_path_with(relative, MATCH_OPTIONS));
            if path.is_dir() && !in_node_modules && !excluded && !dirs.contains(&path) {
                dirs.push(path);
            }
        }
    }
    Ok(dirs)
}

fn normalize(glob: &str) -> String {
    glob.trim()
        .trim_start_matches("./")
        .trim_end_matches('/')
        .to_owned()
}
