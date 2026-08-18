use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "meetlite", about = "A lightweight local meeting recorder")]
pub struct Cli {
    /// Override the configuration file path.
    #[arg(long, global = true, env = "MEETLITE_CONFIG")]
    pub config: Option<PathBuf>,

    /// Emit machine-readable progress and errors when supported.
    #[arg(long, global = true)]
    pub json: bool,

    /// Show additional diagnostic output. Secrets are never printed.
    #[arg(short, long, global = true)]
    pub verbose: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Record microphone and system audio into a local WAV file.
    Record(RecordArgs),
    /// Transcribe a file, or record and transcribe when no file is supplied.
    Transcribe(TranscribeArgs),
    /// List available microphone devices.
    Devices,
    /// Initialize or inspect configuration.
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
}

#[derive(Debug, Args)]
pub struct RecordArgs {
    /// Directory where the recording will be written.
    #[arg(short, long)]
    pub output: Option<PathBuf>,

    /// Stop automatically after this many seconds. Omit to stop with Ctrl-C.
    #[arg(long)]
    pub duration: Option<u64>,

    /// Do not capture the default microphone.
    #[arg(long)]
    pub no_microphone: bool,

    /// Do not capture global system audio.
    #[arg(long)]
    pub no_system_audio: bool,

    /// Override the configured microphone gain.
    #[arg(long)]
    pub microphone_gain: Option<f32>,

    /// Override the configured system-audio gain.
    #[arg(long)]
    pub system_gain: Option<f32>,
}

#[derive(Debug, Args)]
pub struct TranscribeArgs {
    /// Existing audio file to transcribe. Omit to record and transcribe live.
    pub input: Option<PathBuf>,

    /// Directory where recording and transcript artifacts will be written.
    #[arg(short, long)]
    pub output: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    /// Create the default configuration file without overwriting an existing file.
    Init,
    /// Print the effective configuration path.
    Path,
}
