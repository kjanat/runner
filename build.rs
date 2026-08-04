//! Build script: read `[package.metadata]` from `Cargo.toml` and expose it
//! as compile-time env vars (Cargo doesn't surface custom metadata to the
//! crate otherwise). Emits `RUNNER_AUTHOR_NAME` (always), `RUNNER_AUTHOR_EMAIL`
//! (when present and non-empty), and `RUNNER_SCHEMA_BASE` (the base URL the
//! schema `$id`s and the scaffolded `#:schema` directive hang off). Consumers
//! read these via `env!` / `option_env!`. It also captures the source
//! revision, compilation target/profile, and compiler version used by the
//! detailed CLI version output.

use std::{env, fs, path::Path, process::Command};

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
    /// Base URL committed schema `$id`s and the scaffolded `#:schema`
    /// directive hang off, e.g. `https://kjanat.github.io/runner/schemas`.
    #[serde(rename = "schema-base")]
    schema_base: String,
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
    emit_target_metadata();
    emit_provenance();

    let manifest_dir = env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR set by cargo");
    let manifest_path = Path::new(&manifest_dir).join("Cargo.toml");
    let raw = fs::read_to_string(&manifest_path).expect("read Cargo.toml");
    let manifest: Manifest = toml::from_str(&raw).expect("parse Cargo.toml");

    let metadata = manifest.package.metadata;
    println!(
        "cargo:rustc-env=RUNNER_SCHEMA_BASE={}",
        metadata.schema_base
    );

    let primary = metadata
        .authors
        .into_iter()
        .next()
        .expect("package.metadata.authors must contain at least one entry");

    println!("cargo:rustc-env=RUNNER_AUTHOR_NAME={}", primary.name);
    if let Some(email) = primary.email.filter(|e| !e.is_empty()) {
        println!("cargo:rustc-env=RUNNER_AUTHOR_EMAIL={email}");
    }
}

fn emit_target_metadata() {
    println!("cargo:rerun-if-env-changed=RUNNER_BUILD_REVISION");
    println!("cargo:rerun-if-env-changed=RUNNER_BUILD_CHANNEL");
    println!("cargo:rerun-if-env-changed=RUNNER_BUILD_DIRTY");
    println!("cargo:rerun-if-changed=.git/HEAD");
    if let Ok(head) = fs::read_to_string(".git/HEAD")
        && let Some(reference) = head.strip_prefix("ref: ")
    {
        println!("cargo:rerun-if-changed=.git/{}", reference.trim());
    }
    println!(
        "cargo:rustc-env=RUNNER_BUILD_TARGET={}",
        env::var("TARGET").unwrap_or_else(|_| "unknown".to_owned())
    );
    println!(
        "cargo:rustc-env=RUNNER_BUILD_PROFILE={}",
        env::var("PROFILE").unwrap_or_else(|_| "unknown".to_owned())
    );
    for (source, destination) in [
        ("HOST", "RUNNER_BUILD_HOST"),
        ("OPT_LEVEL", "RUNNER_BUILD_OPT_LEVEL"),
        ("DEBUG", "RUNNER_BUILD_DEBUG"),
        ("CARGO_CFG_TARGET_ARCH", "RUNNER_BUILD_TARGET_ARCH"),
        ("CARGO_CFG_TARGET_OS", "RUNNER_BUILD_TARGET_OS"),
        ("CARGO_CFG_TARGET_ENV", "RUNNER_BUILD_TARGET_ENV"),
        ("CARGO_CFG_TARGET_FAMILY", "RUNNER_BUILD_TARGET_FAMILY"),
        (
            "CARGO_CFG_TARGET_POINTER_WIDTH",
            "RUNNER_BUILD_TARGET_POINTER_WIDTH",
        ),
        ("CARGO_CFG_TARGET_ENDIAN", "RUNNER_BUILD_TARGET_ENDIAN"),
        ("CARGO_CFG_TARGET_FEATURE", "RUNNER_BUILD_TARGET_FEATURES"),
    ] {
        println!(
            "cargo:rustc-env={destination}={}",
            env::var(source).unwrap_or_else(|_| "unknown".to_owned())
        );
    }
}

fn emit_provenance() {
    let revision = env::var("RUNNER_BUILD_REVISION").ok().or_else(|| {
        Command::new("git")
            .args(["rev-parse", "--short=9", "HEAD"])
            .output()
            .ok()
            .filter(|output| output.status.success())
            .and_then(|output| String::from_utf8(output.stdout).ok())
            .map(|revision| revision.trim().to_owned())
            .filter(|revision| !revision.is_empty())
    });
    println!(
        "cargo:rustc-env=RUNNER_BUILD_REVISION={}",
        revision.as_deref().unwrap_or("unknown")
    );

    let dirty = env::var("RUNNER_BUILD_DIRTY").ok().map_or_else(
        || {
            Command::new("git")
                .args(["status", "--porcelain", "--untracked-files=no"])
                .output()
                .is_ok_and(|output| output.status.success() && !output.stdout.is_empty())
        },
        |value| value == "1" || value.eq_ignore_ascii_case("true"),
    );
    println!("cargo:rustc-env=RUNNER_BUILD_DIRTY={dirty}");

    let package_version = env::var("CARGO_PKG_VERSION").expect("CARGO_PKG_VERSION set by cargo");
    let exact_release_tag = Command::new("git")
        .args(["tag", "--points-at", "HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .is_some_and(|tags| tags.lines().any(|tag| tag == format!("v{package_version}")));
    let channel = env::var("RUNNER_BUILD_CHANNEL").ok().unwrap_or_else(|| {
        // Published crate/source archives have no `.git` directory and are
        // stable by definition. In a checkout, only the clean matching
        // release tag is stable; every post-release commit identifies itself.
        if (revision.is_none() || exact_release_tag) && !dirty {
            "stable".to_owned()
        } else {
            let count = Command::new("git")
                .args(["rev-list", "--count", "HEAD"])
                .output()
                .ok()
                .filter(|output| output.status.success())
                .and_then(|output| String::from_utf8(output.stdout).ok())
                .map_or_else(|| "unknown".to_owned(), |count| count.trim().to_owned());
            format!("dev.{count}")
        }
    });
    assert!(
        channel == "stable"
            || channel
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'.' || byte == b'-'),
        "RUNNER_BUILD_CHANNEL must be `stable` or a SemVer-compatible prerelease identifier"
    );
    println!("cargo:rustc-env=RUNNER_BUILD_CHANNEL={channel}");

    let rustc = env::var_os("RUSTC")
        .and_then(|rustc| Command::new(rustc).arg("--version").output().ok())
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map_or_else(|| "unknown".to_owned(), |version| version.trim().to_owned());
    println!("cargo:rustc-env=RUNNER_BUILD_RUSTC={rustc}");
}
