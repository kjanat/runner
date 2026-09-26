//! The Node.js ecosystem.

pub mod bun;
pub mod manifest;
pub mod npm;
pub mod package_json;
pub mod packages;
pub mod pnpm;
pub mod runtime;
pub mod workspace;
pub mod yarn;

use std::path::{Path, PathBuf};

use runner_core::{BinDirs, BinsCap, CleanCap, Discovery, Piece, Signal, Template, TestCap};

/// The manifest filenames, in resolution order.
pub const MANIFESTS: &[&str] = &["package.json", "package.json5", "package.yaml"];

/// Where every Node package manager installs to.
pub const WRITES: &[&str] = &["node_modules"];

/// Where installed executables live.
pub const BINS: BinsCap = BinsCap {
    dirs: BinDirs::Static(&["node_modules/.bin"]),
};

/// What `clean` removes for any Node project.
pub const CLEAN: CleanCap = CleanCap {
    dir_suffixes: &[],
    framework_dirs: FRAMEWORK_CLEAN,
    dirs: &["node_modules", ".cache", "dist"],
};

/// Framework build directories removed only on explicit opt-in.
pub const FRAMEWORK_CLEAN: &[&str] = &[".next", ".parcel-cache", ".svelte-kit"];

/// The files `node --test` is handed, which it does not find itself.
pub const TEST_FILES: &[&str] = &[
    "test.js",
    "test.mjs",
    "test.cjs",
    "test.ts",
    "test.mts",
    "test.cts",
    "*.test.js",
    "*.test.mjs",
    "*.test.cjs",
    "*.test.ts",
    "*.test.mts",
    "*.test.cts",
];

/// Node's own test runner, which every Node package manager reaches for.
pub const TEST: TestCap = TestCap {
    program: Some("node"),
    argv: Template(&[
        Piece::FileFlags,
        Piece::Lit("--test"),
        Piece::Args,
        Piece::Files,
    ]),
    discovery: Discovery::Files(TEST_FILES),
    file_flags: Some(strip_types),
};

/// `--experimental-strip-types` when any discovered test file is TypeScript.
fn strip_types(files: &[PathBuf]) -> &'static [&'static str] {
    let typescript = |file: &Path| {
        file.extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| matches!(ext, "ts" | "mts" | "cts"))
    };
    if files.iter().any(|file| typescript(file)) {
        &["--experimental-strip-types"]
    } else {
        &[]
    }
}

/// The two manifest fields that name a package manager.
#[must_use]
pub const fn manifest_signals(
    package_manager: fn(&runner_core::Field<'_>) -> Option<runner_core::Declared>,
    dev_engines: fn(&runner_core::Field<'_>) -> Option<runner_core::Declared>,
) -> [Signal; 2] {
    [
        Signal::ManifestField {
            files: MANIFESTS,
            path: "packageManager",
            parse: package_manager,
        },
        Signal::ManifestField {
            files: MANIFESTS,
            path: "devEngines.packageManager",
            parse: dev_engines,
        },
    ]
}
