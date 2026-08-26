use std::path::Path;

use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait};

use crate::{
    cli::{Cli, Command, ConfigCommand, RecordArgs, SummarizeArgs, TranscribeArgs},
    config::Config,
    recording, summary, transcription,
};

pub fn run(cli: Cli) -> Result<()> {
    let Cli {
        config,
        json,
        verbose: _,
        command,
    } = cli;
    match command {
        #[cfg(target_os = "macos")]
        Command::CaptureAgent(args) => recording::run_capture_agent(args.port, args.token)?,
        Command::Config {
            command: ConfigCommand::Init,
        } => {
            let path = Config::initialize(config.as_deref())?;
            println!("Created configuration at {}", path.display());
        }
        Command::Config {
            command: ConfigCommand::Path,
        } => {
            println!("{}", Config::path(config.as_deref())?.display());
        }
        Command::Devices => list_devices()?,
        Command::Record(args) => run_record(args, config.as_deref(), json)?,
        Command::Transcribe(args) => run_transcribe(args, config.as_deref(), json)?,
        Command::Summarize(args) => run_summarize(args, config.as_deref(), json)?,
    }
    Ok(())
}

fn run_record(args: RecordArgs, config_path: Option<&Path>, json: bool) -> Result<()> {
    if args.transcribe || args.summarize {
        run_live_transcription(args, config_path, json)
    } else {
        let config = Config::load_if_present(config_path)?;
        recording::record(args, config.as_ref().map(|config| &config.recording))
    }
}

fn run_live_transcription(args: RecordArgs, config_path: Option<&Path>, json: bool) -> Result<()> {
    let summarize_after = args.summarize;
    let force = args.force;
    let config = Config::load(config_path)?;
    let transcript =
        transcription::transcribe_live(args, Some(&config.recording), config.stt()?.clone())?;
    if summarize_after {
        let transcript_path = Path::new(&transcript.source_path)
            .parent()
            .context("live recording audio path must include a parent directory")?
            .join("transcript.json");
        let output = summary::summarize(&transcript_path, config.llm.as_ref(), force)?;
        print_json_or_path(json, &output, &output.summary_path)
    } else if json {
        println!("{}", serde_json::to_string_pretty(&transcript)?);
        Ok(())
    } else {
        Ok(())
    }
}

fn run_transcribe(args: TranscribeArgs, config_path: Option<&Path>, json: bool) -> Result<()> {
    let config = Config::load(config_path)?;
    let transcript = transcription::transcribe_file(
        &args.input,
        args.output.as_deref(),
        config.stt()?,
        args.force,
    )?;
    if json {
        println!("{}", serde_json::to_string_pretty(&transcript)?);
    } else {
        println!("{}", transcript.text);
    }
    Ok(())
}

fn run_summarize(args: SummarizeArgs, config_path: Option<&Path>, json: bool) -> Result<()> {
    let config = Config::load(config_path)?;
    let output = summary::summarize(&args.input, config.llm.as_ref(), args.force)?;
    print_json_or_path(json, &output, &output.summary_path)
}

fn print_json_or_path(json: bool, output: &impl serde::Serialize, path: &Path) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(output)?);
    } else {
        println!("{}", path.display());
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
