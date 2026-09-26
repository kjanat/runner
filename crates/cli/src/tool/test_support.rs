//! Shared test-only helpers for tool module unit tests.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_ID: AtomicUsize = AtomicUsize::new(0);

/// Temporary directory wrapper removed on drop.
pub(crate) struct TempDir {
    path: PathBuf,
}

impl TempDir {
    /// Create a uniquely named temp directory with the given `prefix`.
    pub(crate) fn new(prefix: &str) -> Self {
        let pid = std::process::id();

        for _ in 0..1024 {
            let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!("runner-{prefix}-{pid}-{id}"));
            match fs::create_dir(&path) {
                Ok(()) => return Self { path },
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => panic!("temp dir should be created: {e}"),
            }
        }

        panic!("temp dir should be created")
    }

    /// Borrow the temporary directory path.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

thread_local! {
    static PROJECTS: std::cell::RefCell<Vec<TempDir>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// A fixture retained until the test thread exits.
pub(crate) fn project_root() -> PathBuf {
    PROJECTS.with(|projects| {
        let dir = TempDir::new("observed-project");
        let path = dir.path().to_owned();
        projects.borrow_mut().push(dir);
        path
    })
}

/// Declare the provider labelled `label` in a fixture and observe it again.
pub(crate) fn declare(ctx: &mut crate::types::ProjectContext, label: &str) {
    write_signal(&ctx.root, label);
    seed_context(ctx);
}

/// Write the first file signal of the provider labelled `label` into `root`.
pub(crate) fn write_signal(root: &Path, label: &str) {
    use runner_core::Signal;
    assert!(
        root.starts_with(std::env::temp_dir()),
        "fixture must be temporary"
    );
    let provider = runner_providers::REGISTRY.by_label(label).unwrap();
    let Some(name) = provider.signals.iter().find_map(|signal| match signal {
        Signal::File(name)
        | Signal::FileCaseless(name)
        | Signal::FileUpwards(name)
        | Signal::Lockfile(name) => Some(name),
        _ => None,
    }) else {
        return;
    };
    let path = root.join(name);
    if path.exists() {
        return;
    }
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let body = if path
        .extension()
        .is_some_and(|ext| ext == "json" || ext == "jsonc")
    {
        "{}"
    } else {
        ""
    };
    fs::write(path, body).unwrap();
}

/// Declare the sources of a unit fixture's tasks and observe it, with the
/// fixture's own tasks.
pub(crate) fn seed_context(ctx: &mut crate::types::ProjectContext) {
    seed_context_with(ctx, &crate::resolver::ResolutionOverrides::default());
}

/// [`seed_context`] under the invocation's `overrides`.
pub(crate) fn seed_context_with(
    ctx: &mut crate::types::ProjectContext,
    overrides: &crate::resolver::ResolutionOverrides,
) {
    for task in &ctx.tasks {
        write_signal(&ctx.root, task.source.label());
    }
    ctx.project = crate::detect::observe(ctx, overrides)
        .map(|mut project| {
            project.tasks = ctx
                .tasks
                .iter()
                .filter_map(crate::commands::run::core::task)
                .collect();
            project
        })
        .map_err(|error| crate::types::Unobserved {
            kind: error.kind(),
            message: error.to_string(),
        });
}
