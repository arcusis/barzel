use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::Path;

/// Simple file-based cache key generator.
/// In a real implementation this would hash the AST or use a proper content hash.
#[allow(dead_code)]
pub fn compute_project_hash(project_root: &Path) -> u64 {
    let mut hasher = DefaultHasher::new();

    // Hash Cargo.toml if it exists
    let cargo_toml = project_root.join("Cargo.toml");
    if let Ok(metadata) = std::fs::metadata(&cargo_toml) {
        metadata.len().hash(&mut hasher);
        if let Ok(modified) = metadata.modified() {
            modified.hash(&mut hasher);
        }
    }

    // Hash src directory recursively (simplified)
    if let Ok(entries) = std::fs::read_dir(project_root.join("src")) {
        for entry in entries.flatten() {
            if let Ok(metadata) = entry.metadata() {
                metadata.len().hash(&mut hasher);
            }
        }
    }

    hasher.finish()
}

/// Check if we can skip running a layer based on cache
#[allow(dead_code)]
pub fn should_skip_layer(project_root: &Path, _layer: &str, last_hash: u64) -> bool {
    let current_hash = compute_project_hash(project_root);
    current_hash == last_hash
}
