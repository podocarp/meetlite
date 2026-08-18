mod cli;
mod config;
mod recording;

use anyhow::{bail, Context, Result};
use clap::Parser;
use cli::{Cli, Command, ConfigCommand};
use config::Config;
use cpal::traits::{DeviceTrait, HostTrait};

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Config {
            command: ConfigCommand::Init,
        } => {
            let path = Config::initialize(cli.config.as_deref())?;
            println!("Created configuration at {}", path.display());
        }
        Command::Config {
            command: ConfigCommand::Path,
        } => {
            println!("{}", Config::path(cli.config.as_deref())?.display());
        }
        Command::Devices => list_devices()?,
        Command::Record(args) => {
            let config = Config::load_if_present(cli.config.as_deref())?;
            recording::record(args, config.as_ref().map(|config| &config.recording))?;
        }
        Command::Transcribe(_) => {
            let config = Config::load(cli.config.as_deref())?;
            config.stt()?;
            bail!("transcription is not implemented yet; the recorder is the next milestone")
        }
    }

    Ok(())
}

fn list_devices() -> Result<()> {
    let host = cpal::default_host();

    println!("Microphones:");
    let devices = host
        .input_devices()
        .context("could not enumerate microphone devices")?;
    let mut found = false;

    for device in devices {
        found = true;
        let name = device
            .name()
            .unwrap_or_else(|_| "<unavailable name>".into());
        let default_marker = host
            .default_input_device()
            .and_then(|default| default.name().ok())
            .is_some_and(|default_name| default_name == name);
        println!(
            "  {}{}",
            name,
            if default_marker { " (default)" } else { "" }
        );
    }

    if !found {
        println!("  No microphone devices found.");
    }

    #[cfg(target_os = "macos")]
    println!("\nSystem audio:\n  Default system output (native Core Audio capture is pending)");

    #[cfg(not(target_os = "macos"))]
    println!("\nSystem audio:\n  Not implemented for this platform.");

    Ok(())
}
