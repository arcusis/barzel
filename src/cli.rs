use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "barzel")]
#[command(author, version, about = "The unbreakable testing CLI", long_about = None)]
#[command(propagate_version = true)]
pub struct Cli {
    /// Run in stdio JSON mode (for AI agents)
    #[arg(long, global = true)]
    pub stdio: bool,

    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Initialize Barzel in the current (or specified) directory
    Init {
        /// Target directory (defaults to current directory)
        path: Option<PathBuf>,
    },

    /// Run the full verification suite (or specific layers)
    Run {
        /// Target directory (defaults to current directory)
        #[arg(long)]
        path: Option<PathBuf>,

        /// Specific layers to run (logic, structural, hostile)
        #[arg(long, value_delimiter = ',')]
        layer: Option<Vec<String>>,

        /// Skip cache and force re-run of all layers
        #[arg(long)]
        no_cache: bool,

        /// Stop on first critical finding
        #[arg(long)]
        fail_fast: bool,
    },

    /// Show the last report or a specific report
    Report {
        /// Report ID prefix or "latest" (default: latest)
        id: Option<String>,
    },

    /// Check which tools are installed and what runners are available
    Check {
        /// Target directory (defaults to current directory)
        #[arg(long)]
        path: Option<PathBuf>,
    },
}
