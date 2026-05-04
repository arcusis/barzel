use crate::config::BarzelConfig;
use crate::detect::detect_project;
use crate::error::Result;
use owo_colors::OwoColorize;
use std::path::Path;

pub fn run_init(target: Option<&Path>, stdio: bool) -> Result<()> {
    let target_path = target.unwrap_or_else(|| Path::new("."));
    let info = detect_project(target_path)?;

    if !stdio {
        println!(
            "{} Detected {} project at {}",
            "→".bright_blue(),
            info.language.to_string().bright_green(),
            info.root.bright_yellow()
        );
    }

    let config = BarzelConfig::from_project_info(&info);
    let config_path = target_path.join(".barzel.toml");

    if config_path.exists() {
        if !stdio {
            println!(
                "{} .barzel.toml already exists — skipping (use --force to overwrite)",
                "⚠".bright_yellow()
            );
        }
        return Ok(());
    }

    config.save(&config_path)?;

    let barzel_dir = target_path.join(".barzel");
    std::fs::create_dir_all(&barzel_dir)?;

    if !stdio {
        println!(
            "{} Created {}",
            "✓".bright_green(),
            ".barzel.toml".bright_cyan()
        );
        println!(
            "{} Created {}",
            "✓".bright_green(),
            ".barzel/".bright_cyan()
        );
        println!();
        println!("Next steps:");
        println!("  {}  barzel run", "→".bright_blue());
        println!("  {}  barzel run --layer logic", "→".bright_blue());
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn init_creates_barzel_toml() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("Cargo.toml"), b"[package]\nname=\"test\"").unwrap();
        run_init(Some(dir.path()), true).unwrap();
        assert!(dir.path().join(".barzel.toml").exists());
    }

    #[test]
    fn init_creates_barzel_directory() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("Cargo.toml"), b"[package]\nname=\"test\"").unwrap();
        run_init(Some(dir.path()), true).unwrap();
        assert!(dir.path().join(".barzel").exists());
    }

    #[test]
    fn init_skips_if_toml_already_exists() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("Cargo.toml"), b"[package]\nname=\"test\"").unwrap();
        // First init
        run_init(Some(dir.path()), true).unwrap();
        // Overwrite .barzel.toml with sentinel content
        fs::write(dir.path().join(".barzel.toml"), b"# sentinel").unwrap();
        // Second init should skip (not overwrite)
        run_init(Some(dir.path()), true).unwrap();
        let content = fs::read_to_string(dir.path().join(".barzel.toml")).unwrap();
        assert!(content.contains("sentinel"));
    }

    #[test]
    fn init_uses_current_dir_when_no_path() {
        // This just checks it doesn't panic/error when path is None
        // (it will use `.` which exists)
        let result = run_init(None, true);
        assert!(result.is_ok());
    }

    #[test]
    fn init_works_for_typescript_project() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("package.json"), br#"{"name":"my-app"}"#).unwrap();
        run_init(Some(dir.path()), true).unwrap();
        assert!(dir.path().join(".barzel.toml").exists());
        let content = fs::read_to_string(dir.path().join(".barzel.toml")).unwrap();
        assert!(content.contains("typescript") || content.contains("my-app"));
    }

    #[test]
    fn init_toml_contains_project_name() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("Cargo.toml"), b"[package]\nname=\"my-crate\"").unwrap();
        run_init(Some(dir.path()), true).unwrap();
        let content = fs::read_to_string(dir.path().join(".barzel.toml")).unwrap();
        assert!(content.contains("my-crate"));
    }
}
