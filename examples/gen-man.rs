//! Emit roff man pages for the `runner` and `run` CLIs to disk.
//!
//! Run via:
//!
//! ```bash
//! cargo run --example gen-man --features man-gen
//! ```
//!
//! Writes `runner.1`, `run.1`, and one `runner-<sub>.1` per subcommand into
//! `man/` (relative to the workspace root) and prints a one-line summary.
//! CI can verify the committed pages are in sync by re-running the example
//! and asserting `git diff --exit-code man/`.
//!
//! The pages are committed so every distribution channel ships them as plain
//! files: the AUR packages install them to `/usr/share/man/man1/`, the npm
//! facade references them via its `man` field, and `install.sh` drops them in
//! the user man path — all without `clap_mangen` in the shipped binary.

use std::fs;
use std::path::Path;

fn main() {
    let out = Path::new(env!("CARGO_MANIFEST_DIR")).join("man");
    fs::create_dir_all(&out).expect("man dir should be creatable");

    let pages = runner::man_pages();
    for (stem, roff) in &pages {
        let path = out.join(format!("{stem}.1"));
        fs::write(&path, roff).expect("man page should be writable");
    }

    println!("wrote {} man pages to {}", pages.len(), out.display());
}
