//! Build script: pick the primary entry from `[[package.metadata.authors]]`
//! in `Cargo.toml` and expose `RUNNER_AUTHOR_NAME` (always) and
//! `RUNNER_AUTHOR_EMAIL` (when present and non-empty) as compile-time env
//! vars. Consumers read these via `env!` / `option_env!`.

use std::{env, fs, path::Path};

use serde::Deserialize;

#[derive(Deserialize)]
struct Manifest {
    package: Package,
}

#[derive(Deserialize)]
struct Package {
    metadata: Metadata,
}

#[derive(Deserialize)]
struct Metadata {
    authors: Vec<Author>,
}

#[derive(Deserialize)]
struct Author {
    name: String,
    email: Option<String>,
}

/// Reads the package metadata from Cargo.toml, selects the first author entry, and exports
/// the author's name (and, if present and non-empty, email) as compile-time environment variables
/// for dependent crates.
///
/// This build script also instructs Cargo to re-run the build script when Cargo.toml changes.
/// It will panic if `CARGO_MANIFEST_DIR` is not set, if Cargo.toml cannot be read or parsed,
/// or if `package.metadata.authors` is empty.
///
/// # Examples
///
/// ```no_run
/// // When run as a build script, this prints lines like:
/// // cargo:rerun-if-changed=Cargo.toml
/// // cargo:rustc-env=RUNNER_AUTHOR_NAME=Alice
/// // cargo:rustc-env=RUNNER_AUTHOR_EMAIL=alice@example.com
/// main();
/// ```
fn main() {
    println!("cargo:rerun-if-changed=Cargo.toml");

    let manifest_dir = env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR set by cargo");
    let manifest_path = Path::new(&manifest_dir).join("Cargo.toml");
    let raw = fs::read_to_string(&manifest_path).expect("read Cargo.toml");
    let manifest: Manifest = toml::from_str(&raw).expect("parse Cargo.toml");

    let primary = manifest
        .package
        .metadata
        .authors
        .into_iter()
        .next()
        .expect("package.metadata.authors must contain at least one entry");

    println!("cargo:rustc-env=RUNNER_AUTHOR_NAME={}", primary.name);
    if let Some(email) = primary.email.filter(|e| !e.is_empty()) {
        println!("cargo:rustc-env=RUNNER_AUTHOR_EMAIL={email}");
    }
}
