//! Write `man/*.1`: `cargo run --example gen-man --features man-gen`.

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
