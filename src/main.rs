mod cli;
mod config;
mod recording;
mod setup;
mod transcription;

use anyhow::{Context, Result};
use clap::Parser;
use cli::{Cli, Command, ConfigCommand};
use config::Config;
use cpal::traits::{DeviceTrait, HostTrait};

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

    match cli.command {
        #[cfg(target_os = "macos")]
        Command::CaptureAgent(args) => recording::run_capture_agent(args.port, args.token)?,
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
        Command::Setup => setup::run()?,
        Command::Record(args) => {
            let config = Config::load_if_present(cli.config.as_deref())?;
            recording::record(args, config.as_ref().map(|config| &config.recording))?;
        }
        Command::Transcribe(args) => {
            let config = Config::load(cli.config.as_deref())?;
            let transcript = match args.input.as_deref() {
                Some(input) => {
                    transcription::transcribe_file(input, args.output.as_deref(), config.stt()?)?
                }
                None => transcription::transcribe_live(
                    args,
                    Some(&config.recording),
                    config.stt()?.clone(),
                )?,
            };
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&transcript)?);
            } else {
                println!("{}", transcript.text);
            }
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
    println!("\nSystem audio:\n  Default system output (captured by MeetliteCapture.app)");

    #[cfg(target_os = "linux")]
    println!(
        "\nSystem audio:\n  Default PulseAudio monitor, with recording.system_device as an ALSA fallback."
    );

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    println!("\nSystem audio:\n  Not implemented for this platform.");

    Ok(())
}
