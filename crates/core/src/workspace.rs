//! Workspaces: the members a root declares and the one an invocation sits in.

use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};

use crate::registry::Registry;
use crate::scope::Scope;
use crate::warning::Warning;

/// One file's workspace declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declaration {
    /// The declaring file and field, e.g. `pnpm-workspace.yaml`.
    pub kind: &'static str,
    /// The member directories carrying the manifest the declaration implies,
    /// each with the name that manifest declares.
    pub members: Vec<(Option<String>, PathBuf)>,
}

/// A workspace member.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    /// The declared name, or the directory name when none is declared.
    pub name: String,
    /// Directory relative to the workspace root, forward slashes.
    pub path: String,
    /// The token addressing this member and no other: `name` unless a
    /// sibling shares it, else `path`.
    pub label: String,
    /// Absolute directory.
    pub dir: PathBuf,
}

/// The workspace a root declares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workspace {
    /// The directory holding the declarations.
    pub root: PathBuf,
    /// Every declaration found, in registry order.
    pub kinds: Vec<&'static str>,
    /// Members in path order.
    pub members: Vec<Member>,
    /// The index of the member holding the invocation directory.
    pub current: Option<usize>,
}

impl Workspace {
    /// Every member as a scope named by its label.
    #[must_use]
    pub fn scopes(&self) -> Vec<Scope> {
        self.members
            .iter()
            .map(|member| Scope::Member {
                name: member.label.clone(),
                dir: member.dir.clone(),
            })
            .collect()
    }

    /// The member holding the invocation directory.
    #[must_use]
    pub fn current(&self) -> Option<&Member> {
        self.current.and_then(|index| self.members.get(index))
    }
}

/// The workspace declared at `root`, from every provider's
/// [`crate::WorkspaceCap`]. A declaration two providers share counts once.
///
/// # Errors
///
/// Returns the first declaration that cannot be read.
pub fn discover(root: &Path, registry: &Registry) -> Result<Option<Workspace>, Warning> {
    let mut kinds: Vec<&'static str> = Vec::new();
    let mut members: Vec<Member> = Vec::new();
    for provider in registry.iter() {
        let Some(cap) = provider.caps.workspaces else {
            continue;
        };
        for declaration in (cap.declarations)(root)? {
            if kinds.contains(&declaration.kind) {
                continue;
            }
            kinds.push(declaration.kind);
            for (declared, dir) in declaration.members {
                if members.iter().any(|member| member.dir == dir) {
                    continue;
                }
                let path = relative(root, &dir);
                let name = declared.filter(|name| !name.is_empty()).unwrap_or_else(|| {
                    dir.file_name()
                        .map_or_else(|| path.clone(), |name| name.to_string_lossy().into_owned())
                });
                members.push(Member {
                    label: name.clone(),
                    name,
                    path,
                    dir,
                });
            }
        }
    }
    if kinds.is_empty() {
        return Ok(None);
    }
    members.sort_by(|a, b| a.path.cmp(&b.path));
    disambiguate(&mut members);
    Ok(Some(Workspace {
        root: root.to_path_buf(),
        kinds,
        members,
        current: None,
    }))
}

/// The workspace `dir` belongs to.
///
/// That is the one declared in `dir`, else the nearest ancestor inside
/// `boundary` declaring one. A `standalone` directory, one with project files
/// of its own, skips a workspace it is no member of.
///
/// # Errors
///
/// Returns the first declaration that cannot be read.
pub fn anchor(
    dir: &Path,
    boundary: Option<&Path>,
    standalone: bool,
    registry: &Registry,
) -> Result<Option<Workspace>, Warning> {
    if let Some(workspace) = discover(dir, registry)? {
        return Ok(Some(workspace));
    }
    for ancestor in dir
        .ancestors()
        .skip(1)
        .take_while(|ancestor| boundary.is_none_or(|boundary| ancestor.starts_with(boundary)))
    {
        let Some(mut workspace) = discover(ancestor, registry)? else {
            continue;
        };
        workspace.current = workspace
            .members
            .iter()
            .position(|member| dir.starts_with(&member.dir));
        if workspace.current.is_none() && standalone {
            continue;
        }
        return Ok(Some(workspace));
    }
    Ok(None)
}

/// Relabel every member whose name a sibling shares to its path.
fn disambiguate(members: &mut [Member]) {
    let mut counts: HashMap<&str, usize> = HashMap::with_capacity(members.len());
    for member in &*members {
        *counts.entry(member.name.as_str()).or_default() += 1;
    }
    let shared: HashSet<String> = counts
        .into_iter()
        .filter(|&(_, count)| count > 1)
        .map(|(name, _)| name.to_owned())
        .collect();
    for member in members {
        if shared.contains(&member.name) {
            member.label.clone_from(&member.path);
        }
    }
}

fn relative(root: &Path, dir: &Path) -> String {
    dir.strip_prefix(root)
        .unwrap_or(dir)
        .components()
        .filter_map(|component| match component {
            Component::Normal(segment) => Some(segment.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{Declaration, anchor, discover};
    use crate::capability::{Capabilities, WorkspaceCap};
    use crate::probe::tests::TempDir;
    use crate::provider::{Ecosystem, Hooks, Kind, ProviderId};
    use crate::registry::{Provider, Registry};
    use crate::warning::Warning;

    fn declared(root: &Path) -> Result<Vec<Declaration>, Warning> {
        if root.join("ws").is_dir() {
            return Err(Warning::general("ws is a directory"));
        }
        if !root.join("ws").is_file() {
            return Ok(Vec::new());
        }
        let members = ["apps/web", "tools/web", "libs/core"]
            .into_iter()
            .map(|path| root.join(path))
            .filter(|dir| dir.is_dir())
            .map(|dir| {
                let name = dir.file_name().map(|n| n.to_string_lossy().into_owned());
                (name, dir)
            })
            .collect();
        Ok(vec![Declaration {
            kind: "ws",
            members,
        }])
    }

    const fn provider(id: ProviderId) -> Provider {
        Provider {
            id,
            label: "fake",
            aliases: &[],
            ecosystem: Ecosystem::Node,
            kind: Kind::PACKAGE_MANAGER,
            program: None,
            signals: &[],
            caps: Capabilities {
                workspaces: Some(WorkspaceCap {
                    declarations: declared,
                }),
                ..Capabilities::NONE
            },
            tasks: None,
            version: None,
            hooks: Hooks::NONE,
        }
    }

    static PROVIDERS: &[Provider] = &[provider(ProviderId::Npm), provider(ProviderId::Pnpm)];

    fn fixture(name: &str) -> TempDir {
        let dir = TempDir::new(name);
        std::fs::write(dir.path().join("ws"), "").unwrap();
        for member in ["apps/web", "tools/web", "libs/core/src"] {
            std::fs::create_dir_all(dir.path().join(member)).unwrap();
        }
        dir
    }

    #[test]
    fn a_shared_declaration_is_read_once_and_clashing_names_take_their_paths() {
        let dir = fixture("workspace-discover");
        let workspace = discover(dir.path(), &Registry(PROVIDERS)).unwrap().unwrap();
        assert_eq!(workspace.kinds, ["ws"]);
        let labels: Vec<&str> = workspace.members.iter().map(|m| m.label.as_str()).collect();
        assert_eq!(labels, ["apps/web", "core", "tools/web"]);
    }

    #[test]
    fn a_member_directory_anchors_to_its_workspace_and_a_standalone_one_does_not() {
        let dir = fixture("workspace-anchor");
        let inside = dir.path().join("libs/core/src");
        let workspace = anchor(&inside, Some(dir.path()), true, &Registry(PROVIDERS))
            .unwrap()
            .unwrap();
        assert_eq!(workspace.current().map(|m| m.name.as_str()), Some("core"));
        let outside = dir.path().join("docs");
        std::fs::create_dir_all(&outside).unwrap();
        assert_eq!(
            anchor(&outside, Some(dir.path()), true, &Registry(PROVIDERS)).unwrap(),
            None
        );
        let loose = anchor(&outside, Some(dir.path()), false, &Registry(PROVIDERS))
            .unwrap()
            .unwrap();
        assert_eq!(loose.root, PathBuf::from(dir.path()));
        assert_eq!(loose.current, None);
    }

    #[test]
    fn an_unreadable_declaration_stops_the_anchor() {
        let dir = TempDir::new("workspace-unreadable");
        std::fs::create_dir_all(dir.path().join("ws")).unwrap();
        let member = dir.path().join("apps/web");
        std::fs::create_dir_all(&member).unwrap();
        assert!(anchor(&member, Some(dir.path()), false, &Registry(PROVIDERS)).is_err());
    }
}
