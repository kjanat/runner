use std::path::Path;

pub const CLEAN_DIRS: &[&str] = &[".nx"];

pub fn detect(dir: &Path) -> bool {
    dir.join("nx.json").exists()
}
