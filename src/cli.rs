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

        /// Overwrite an existing .barzel.toml
        #[arg(long)]
        force: bool,
    },

    /// Run the full verification suite (or specific layers)
    Run {
        /// Target directory (defaults to current directory)
        #[arg(long)]
        path: Option<PathBuf>,

        /// Specific layers to run (logic, structural, hostile, operational)
        #[arg(long, value_delimiter = ',')]
        layer: Option<Vec<String>>,

        /// Skip cache and force re-run of all layers
        #[arg(long)]
        no_cache: bool,

        /// Stop on first critical finding
        #[arg(long)]
        fail_fast: bool,

        /// Output the full JSON report to stdout (for scripting)
        #[arg(long)]
        json: bool,

        /// Only verify packages/projects affected since this git revision
        /// (e.g. --since HEAD~1, --since main, --since abc123).
        /// Always runs security/audit runners when lockfiles change.
        #[arg(long)]
        since: Option<String>,
    },

    /// Show the last report or a specific report; compare two reports for regressions
    Report {
        /// Report ID prefix or "latest" (default: latest)
        id: Option<String>,

        /// Compare two reports: --compare <baseline-id> <head-id>
        /// IDs may be prefixes or "latest". Outputs regression/improvement summary.
        #[arg(long, num_args = 2, value_names = ["BASELINE", "HEAD"])]
        compare: Option<Vec<String>>,

        /// Output compare result as JSON (only used with --compare)
        #[arg(long)]
        json: bool,
    },

    /// Check which tools are installed and what runners are available
    Check {
        /// Target directory (defaults to current directory)
        #[arg(long)]
        path: Option<PathBuf>,
    },
}
