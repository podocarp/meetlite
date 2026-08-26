mod cli;
mod config;
mod pipeline;
mod recording;
mod setup;
mod summary;
mod transcription;

use anyhow::Result;
use clap::Parser;
use cli::Cli;

fn main() -> Result<()> {
    // LaunchServices passes a legacy process-serial argument to app executables.
    // It is not part of Meetlite's CLI contract and would otherwise prevent the
    // hidden capture-agent command from starting.
    #[cfg(target_os = "macos")]
    let arguments =
        std::env::args_os().filter(|argument| !argument.to_string_lossy().starts_with("-psn_"));
    #[cfg(target_os = "macos")]
    let cli = Cli::parse_from(arguments);
    #[cfg(not(target_os = "macos"))]
    let cli = Cli::parse();

    pipeline::run(cli)
}
