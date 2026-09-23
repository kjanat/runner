//! The run cascade.

use crate::reach::Reach;

/// A capability a rung needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Cap {
    /// An exec primitive.
    Exec,
    /// A test runner.
    Test,
}

/// What a rung needs to take a name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Need {
    /// The name is one of runner's own verbs.
    BareVerb,
    /// The name is a path with a separator.
    ExplicitPath,
    /// A declared task with that name.
    Task,
    /// A file with that name relative to the invocation directory.
    RelativeFile,
    /// An installed dependency exposing that binary.
    InstalledDep,
    /// A present provider with the capability.
    Cap(Cap),
    /// A project bin dir holding that name.
    ProjectBins,
    /// The name on the user's `PATH`.
    HostPath,
    /// A tool manager that can exec the name.
    ToolManagerExec,
}

/// One step of the cascade.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rung {
    /// The rung's name in reports.
    pub name: &'static str,
    /// What it needs.
    pub needs: Need,
    /// Whether it can fetch.
    pub reach: Reach,
}

/// The cascade, local rungs first.
pub static CASCADE: &[Rung] = &[
    Rung {
        name: "builtin",
        needs: Need::BareVerb,
        reach: Reach::Local,
    },
    Rung {
        name: "path",
        needs: Need::ExplicitPath,
        reach: Reach::Local,
    },
    Rung {
        name: "task",
        needs: Need::Task,
        reach: Reach::Local,
    },
    Rung {
        name: "file",
        needs: Need::RelativeFile,
        reach: Reach::Local,
    },
    Rung {
        name: "dep",
        needs: Need::InstalledDep,
        reach: Reach::Local,
    },
    Rung {
        name: "test",
        needs: Need::Cap(Cap::Test),
        reach: Reach::Local,
    },
    Rung {
        name: "bins",
        needs: Need::ProjectBins,
        reach: Reach::Local,
    },
    Rung {
        name: "host",
        needs: Need::HostPath,
        reach: Reach::Local,
    },
    Rung {
        name: "manager",
        needs: Need::ToolManagerExec,
        reach: Reach::Network,
    },
    Rung {
        name: "exec",
        needs: Need::Cap(Cap::Exec),
        reach: Reach::Network,
    },
];

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::CASCADE;

    #[test]
    fn local_rungs_precede_every_network_rung() {
        assert!(
            CASCADE
                .windows(2)
                .all(|pair| pair[0].reach <= pair[1].reach)
        );
    }

    #[test]
    fn rung_names_are_unique() {
        let names: HashSet<&str> = CASCADE.iter().map(|rung| rung.name).collect();
        assert_eq!(names.len(), CASCADE.len());
    }
}
