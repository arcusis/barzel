mod cli;
mod config;
mod detect;
mod error;
mod init;
mod report;
mod run;

use chrono::Utc;
use clap::Parser;
use cli::{Cli, Commands};
use owo_colors::OwoColorize;
use std::io::{self, Read};
use std::process::ExitCode;
use uuid::Uuid;

#[derive(serde::Deserialize)]
struct StdioRequest {
    command: String,
    #[serde(default)]
    project_path: Option<String>,
    #[serde(default)]
    layers: Option<Vec<String>>,
    #[serde(default)]
    request_id: Option<String>,
}

#[derive(serde::Serialize)]
struct StdioResponse {
    status: String,
    request_id: String,
    timestamp: String,
    version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

fn create_response(
    status: &str,
    request_id: Option<String>,
    data: Option<serde_json::Value>,
    error: Option<String>,
) -> StdioResponse {
    StdioResponse {
        status: status.to_string(),
        request_id: request_id.unwrap_or_else(|| Uuid::new_v4().to_string()),
        timestamp: Utc::now().to_rfc3339(),
        version: "1".to_string(),
        data,
        error,
    }
}

fn handle_stdio() -> ExitCode {
    let mut input = String::new();
    if let Err(e) = io::stdin().read_to_string(&mut input) {
        let resp = create_response("error", None, None, Some(format!("failed to read stdin: {}", e)));
        println!("{}", serde_json::to_string(&resp).unwrap());
        return ExitCode::from(1);
    }

    let req: StdioRequest = match serde_json::from_str(&input) {
        Ok(r) => r,
        Err(e) => {
            let resp = create_response("error", None, None, Some(format!("invalid JSON request: {}", e)));
            println!("{}", serde_json::to_string(&resp).unwrap());
            return ExitCode::from(1);
        }
    };

    let request_id = req.request_id.clone();

    match req.command.as_str() {
        "init" => {
            let path = req.project_path.as_deref().map(std::path::Path::new);
            match init::run_init(path, true) {
                Ok(()) => {
                    let data = serde_json::json!({ "message": "project initialized" });
                    let resp = create_response("success", request_id, Some(data), None);
                    println!("{}", serde_json::to_string(&resp).unwrap());
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    let resp = create_response("error", request_id, None, Some(e.to_string()));
                    println!("{}", serde_json::to_string(&resp).unwrap());
                    ExitCode::from(1)
                }
            }
        }
        "run" => {
            let path = req.project_path.as_deref().map(std::path::Path::new);
            match run::run_verification(path, req.layers, true) {
                Ok(()) => {
                    let data = serde_json::json!({ "message": "verification complete" });
                    let resp = create_response("success", request_id, Some(data), None);
                    println!("{}", serde_json::to_string(&resp).unwrap());
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    let resp = create_response("error", request_id, None, Some(e.to_string()));
                    println!("{}", serde_json::to_string(&resp).unwrap());
                    ExitCode::from(1)
                }
            }
        }
        other => {
            let resp = create_response(
                "error",
                request_id,
                None,
                Some(format!("unknown command: {}", other)),
            );
            println!("{}", serde_json::to_string(&resp).unwrap());
            ExitCode::from(1)
        }
    }
}

fn main() -> ExitCode {
    // Fast-path for AI agents
    let raw_args: Vec<String> = std::env::args().collect();
    if raw_args.iter().any(|a| a == "--stdio") {
        return handle_stdio();
    }

    // Normal CLI
    let cli = Cli::parse();

    let command = match cli.command {
        Some(c) => c,
        None => {
            eprintln!(
                "{} No subcommand provided. Use --help for usage.",
                "Error:".bright_red()
            );
            return ExitCode::from(2);
        }
    };

    let result = match command {
        Commands::Init { path } => init::run_init(path.as_deref(), false),

        Commands::Run { layer, fail_fast: _ } => {
            // Note: clap Run struct doesn't have path in this version — using current dir
            run::run_verification(None, layer, false)
        }

        Commands::Report { id: _ } => {
            println!(
                "{} Report command not yet fully implemented (M1 foundation only)",
                "→".bright_blue()
            );
            println!("Latest report would be shown here.");
            Ok(())
        }
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{} {}", "Error:".bright_red(), e);
            ExitCode::from(1)
        }
    }
}
