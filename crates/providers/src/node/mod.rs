//! The Node.js ecosystem.

pub mod bun;
pub mod manifest;
pub mod npm;
pub mod package_json;
pub mod pnpm;
pub mod runtime;
pub mod workspace;
pub mod yarn;

use runner_core::{BinDirs, BinsCap, CleanCap, Discovery, Signal, TestCap, t};

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
    dirs: &["node_modules", ".cache", "dist"],
};

/// Framework build directories removed only on explicit opt-in.
pub const FRAMEWORK_CLEAN: &[&str] = &[".next", ".parcel-cache", ".svelte-kit"];

/// The files `node --test` is handed, which it does not find itself.
pub const TEST_FILES: &[&str] = &[
    "test.js",
    "test.mjs",
    "test.cjs",
    "test.jsx",
    "test.ts",
    "test.mts",
    "test.cts",
    "test.tsx",
    "*.test.js",
    "*.test.mjs",
    "*.test.cjs",
    "*.test.jsx",
    "*.test.ts",
    "*.test.mts",
    "*.test.cts",
    "*.test.tsx",
];

/// Node's own test runner, which every Node package manager reaches for.
pub const TEST: TestCap = TestCap {
    program: Some("node"),
    argv: t!["--test", Args, Files],
    discovery: Discovery::Files(TEST_FILES),
};

/// The two manifest fields that name a package manager.
#[must_use]
pub const fn manifest_signals(
    package_manager: fn(&serde_json::Value) -> Option<runner_core::Declared>,
    dev_engines: fn(&serde_json::Value) -> Option<runner_core::Declared>,
) -> [Signal; 2] {
    [
        Signal::ManifestField {
            file: "package.json",
            path: "packageManager",
            parse: package_manager,
        },
        Signal::ManifestField {
            file: "package.json",
            path: "devEngines.packageManager",
            parse: dev_engines,
        },
    ]
}
