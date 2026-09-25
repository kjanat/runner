//! The provider declaration and lookup over a provider table.

use crate::capability::Capabilities;
use crate::evidence::{Evidence, Present};
use crate::op::Op;
use crate::provider::{Ecosystem, Hooks, Kind, ProviderId};
use crate::signal::Signal;
use crate::task::Extracted;
use crate::tree::Tree;
use crate::warning::Warning;

/// Task extraction when the format is the tool's own.
pub type TasksFn = fn(&Present, &Tree) -> Result<Extracted, Warning>;

/// Version parsing when `<program> --version` needs a tool-specific parse.
pub type VersionFn = fn(&Present) -> Result<String, Warning>;

/// A warning the core cannot know before a plan is made.
pub type BeforePlanFn = fn(&Present, &Op<'_>, &mut Vec<Warning>) -> Result<(), crate::Refusal>;

/// Evidence derived from other evidence.
pub type AfterObserveFn = fn(&Tree, &[Evidence]) -> std::io::Result<Vec<Evidence>>;

/// One tool runner knows about.
#[derive(Clone, Copy)]
pub struct Provider {
    /// Registry index.
    pub id: ProviderId,
    /// The label users type and reports print.
    pub label: &'static str,
    /// Other spellings `from_label` accepts.
    pub aliases: &'static [&'static str],
    /// The language the provider belongs to.
    pub ecosystem: Ecosystem,
    /// What the provider is.
    pub kind: Kind,
    /// The executable, probed with `PATHEXT` on Windows. `None` for a file-only task source.
    pub program: Option<&'static str>,
    /// What observation looks for.
    pub signals: &'static [Signal],
    /// Install directories this provider materialises.
    pub writes: &'static [&'static str],
    /// What the provider can do.
    pub caps: Capabilities,
    /// Task extraction when the format is the tool's own.
    pub tasks: Option<TasksFn>,
    /// Version parsing when `<program> --version` needs a tool-specific parse.
    pub version: Option<VersionFn>,
    /// The two places provider code runs besides `tasks` and `version`.
    pub hooks: Hooks,
}

impl Provider {
    /// Select the capability table named by this scope's observed variant.
    #[must_use]
    pub fn for_present(&self, present: &Present) -> Self {
        let caps = present
            .because
            .iter()
            .find_map(|evidence| {
                let Some(crate::Declared::Variant(name)) = &evidence.declared else {
                    return None;
                };
                debug_assert!(
                    self.caps.variants.iter().any(|(label, _)| label == name),
                    "{} has no {name} capability table",
                    self.label
                );
                self.caps
                    .variants
                    .iter()
                    .find(|(label, _)| *label == name)
                    .map(|(_, caps)| *caps)
            })
            .unwrap_or(self.caps);
        Self { caps, ..*self }
    }

    /// Whether `spelling` is the label or one of the aliases.
    #[must_use]
    pub fn answers_to(&self, spelling: &str) -> bool {
        self.label == spelling || self.aliases.contains(&spelling)
    }
}

/// A provider table with lookup by id, label or alias.
#[derive(Clone, Copy)]
pub struct Registry(pub &'static [Provider]);

impl Registry {
    /// Capabilities effective for a provider in this scope, with root inheritance.
    #[must_use]
    pub fn effective(
        &self,
        id: ProviderId,
        project: &crate::Project,
        scope: &crate::Scope,
    ) -> Provider {
        let provider = self.by_id(id);
        let observed = project.present_in(id, scope);
        observed.map_or(*provider, |p| provider.for_present(p))
    }

    /// Runtime providers accepting this file under the scope's observed variants.
    #[must_use]
    pub fn file_runtimes(
        &self,
        file: &std::path::Path,
        project: &crate::Project,
        scope: &crate::Scope,
    ) -> Vec<ProviderId> {
        self.of_kind(Kind::RUNTIME)
            .filter_map(|provider| {
                let effective = self.effective(provider.id, project, scope);
                effective
                    .caps
                    .run_file
                    .filter(|cap| cap.supports(file))
                    .filter(|cap| cap.program.or(effective.program).is_some())
                    .map(|_| provider.id)
            })
            .collect()
    }

    /// The provider with `id`.
    ///
    /// # Panics
    ///
    /// When the table has no entry for `id`. The providers crate's drift test rules that out.
    #[must_use]
    pub fn by_id(&self, id: ProviderId) -> &'static Provider {
        self.0
            .iter()
            .find(|provider| provider.id == id)
            .unwrap_or_else(|| panic!("registry has no entry for {id:?}"))
    }

    /// The provider whose label or alias is `spelling`, trimmed.
    #[must_use]
    pub fn by_label(&self, spelling: &str) -> Option<&'static Provider> {
        let spelling = spelling.trim();
        self.0.iter().find(|provider| provider.answers_to(spelling))
    }

    /// Every provider with any of `kind`'s bits.
    pub fn of_kind(&self, kind: Kind) -> impl Iterator<Item = &'static Provider> {
        self.0
            .iter()
            .filter(move |provider| provider.kind.intersects(kind))
    }

    /// Every provider.
    pub fn iter(&self) -> impl Iterator<Item = &'static Provider> {
        self.0.iter()
    }
}
