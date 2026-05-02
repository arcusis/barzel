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
