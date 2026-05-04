use crate::config::BarzelConfig;
use crate::detect::detect_project;
use crate::error::Result;
use owo_colors::OwoColorize;
use std::path::Path;

/// Result of running init, for structured callers (e.g. stdio response).
#[derive(Debug, PartialEq)]
pub enum InitOutcome {
    Created,
    Skipped,
    Overwritten,
}

pub fn run_init(target: Option<&Path>, stdio: bool, force: bool) -> Result<InitOutcome> {
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

    if config_path.exists() && !force {
        if !stdio {
            println!(
                "{} .barzel.toml already exists — skipping (use --force to overwrite)",
                "⚠".bright_yellow()
            );
        }
        return Ok(InitOutcome::Skipped);
    }

    let outcome = if config_path.exists() { InitOutcome::Overwritten } else { InitOutcome::Created };

    config.save(&config_path)?;

    let barzel_dir = target_path.join(".barzel");
    std::fs::create_dir_all(&barzel_dir)?;

    if !stdio {
        let action = match outcome {
            InitOutcome::Overwritten => "Overwrote",
            _ => "Created",
        };
        println!(
            "{} {} {}",
            "✓".bright_green(),
            action,
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

    Ok(outcome)
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
        let outcome = run_init(Some(dir.path()), true, false).unwrap();
        assert!(dir.path().join(".barzel.toml").exists());
        assert_eq!(outcome, InitOutcome::Created);
    }

    #[test]
    fn init_creates_barzel_directory() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("Cargo.toml"), b"[package]\nname=\"test\"").unwrap();
        run_init(Some(dir.path()), true, false).unwrap();
        assert!(dir.path().join(".barzel").exists());
    }

    #[test]
    fn init_skips_if_toml_already_exists_without_force() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("Cargo.toml"), b"[package]\nname=\"test\"").unwrap();
        run_init(Some(dir.path()), true, false).unwrap();
        // Overwrite .barzel.toml with sentinel content
        fs::write(dir.path().join(".barzel.toml"), b"# sentinel").unwrap();
        // Second init without --force should skip
        let outcome = run_init(Some(dir.path()), true, false).unwrap();
        let content = fs::read_to_string(dir.path().join(".barzel.toml")).unwrap();
        assert!(content.contains("sentinel"), "sentinel must be preserved when skipping");
        assert_eq!(outcome, InitOutcome::Skipped);
    }

    #[test]
    fn init_force_overwrites_existing_toml() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("Cargo.toml"), b"[package]\nname=\"test\"").unwrap();
        run_init(Some(dir.path()), true, false).unwrap();
        // Write sentinel
        fs::write(dir.path().join(".barzel.toml"), b"# sentinel").unwrap();
        // Force overwrite
        let outcome = run_init(Some(dir.path()), true, true).unwrap();
        let content = fs::read_to_string(dir.path().join(".barzel.toml")).unwrap();
        assert!(!content.contains("sentinel"), "sentinel must be gone after force overwrite");
        assert_eq!(outcome, InitOutcome::Overwritten);
    }

    #[test]
    fn init_uses_current_dir_when_no_path() {
        let result = run_init(None, true, false);
        assert!(result.is_ok());
    }

    #[test]
    fn init_works_for_typescript_project() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("package.json"), br#"{"name":"my-app"}"#).unwrap();
        run_init(Some(dir.path()), true, false).unwrap();
        assert!(dir.path().join(".barzel.toml").exists());
        let content = fs::read_to_string(dir.path().join(".barzel.toml")).unwrap();
        assert!(content.contains("typescript") || content.contains("my-app"));
    }

    #[test]
    fn init_toml_contains_project_name() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("Cargo.toml"), b"[package]\nname=\"my-crate\"").unwrap();
        run_init(Some(dir.path()), true, false).unwrap();
        let content = fs::read_to_string(dir.path().join(".barzel.toml")).unwrap();
        assert!(content.contains("my-crate"));
    }
}
