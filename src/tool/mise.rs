use std::path::Path;

pub fn detect(dir: &Path) -> bool {
    dir.join("mise.toml").exists() || dir.join(".mise.toml").exists()
}
