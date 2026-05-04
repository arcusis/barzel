use std::path::Path;

/// Resolve a project-local Python venv executable for `name`.
/// Checks POSIX layout first (`.venv/bin/<name>`), then Windows layout
/// (`.venv/Scripts/<name>.exe`). Returns the path string if found, or
/// `None` to fall back to the system PATH.
pub fn venv_tool(root: &Path, name: &str) -> Option<String> {
    let posix = root.join(".venv").join("bin").join(name);
    if posix.exists() {
        return Some(posix.to_string_lossy().into_owned());
    }
    let windows = root.join(".venv").join("Scripts").join(format!("{}.exe", name));
    if windows.exists() {
        return Some(windows.to_string_lossy().into_owned());
    }
    None
}
