mod cli;
mod config;
mod credentials;
mod live_control;
mod output;
mod pipeline;
mod recording;
#[cfg(target_os = "macos")]
mod setup;
mod summary;
mod transcription;

use std::{ffi::OsString, process::ExitCode};

use clap::Parser;
use cli::Cli;
use output::Output;

fn main() -> ExitCode {
    let arguments = arguments();
    let json = arguments.iter().any(|argument| argument == "--json");
    let cli = match Cli::try_parse_from(arguments) {
        Ok(cli) => cli,
        Err(error) if json => {
            let _ = emit_error();
            return ExitCode::from(error.exit_code() as u8);
        }
        Err(error) => {
            let _ = error.print();
            return ExitCode::from(error.exit_code() as u8);
        }
    };

    match pipeline::run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(_error) if json => {
            let _ = emit_error();
            ExitCode::FAILURE
        }
        Err(error) => {
            eprintln!("Error: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn arguments() -> Vec<OsString> {
    #[cfg(target_os = "macos")]
    {
        std::env::args_os()
            .filter(|argument| !argument.to_string_lossy().starts_with("-psn_"))
            .collect()
    }
    #[cfg(not(target_os = "macos"))]
    {
        std::env::args_os().collect()
    }
}

fn emit_error() -> anyhow::Result<()> {
    Output::new(true).event(&serde_json::json!({
        "type": "error",
        "message": "Meetlite command failed",
    }))
}
