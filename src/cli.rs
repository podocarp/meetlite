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
    /// Transcribe an existing audio recording.
    Transcribe(TranscribeArgs),
    /// Generate a Markdown summary from a transcript.
    Summarize(SummarizeArgs),
    /// List available microphone devices.
    Devices,
    /// Initialize or inspect configuration.
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    /// Install or update the macOS system-audio capture companion.
    Setup,
    /// Internal macOS capture companion. Launched only by MeetliteCapture.app.
    #[cfg(target_os = "macos")]
    #[command(hide = true)]
    CaptureAgent(CaptureAgentArgs),
}

#[cfg(target_os = "macos")]
#[derive(Debug, Args)]
pub struct CaptureAgentArgs {
    #[arg(long)]
    pub port: u16,

    #[arg(long)]
    pub token: String,
}

#[derive(Debug, Clone, Args)]
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

    /// Replace existing Meetlite artifacts in the output directory.
    #[arg(long)]
    pub force: bool,

    /// Transcribe the recording while it is captured.
    #[arg(long)]
    pub transcribe: bool,

    /// Transcribe the recording and write a summary when it finishes.
    #[arg(long)]
    pub summarize: bool,
}

#[derive(Debug, Args)]
pub struct TranscribeArgs {
    /// Existing audio file to transcribe.
    pub input: PathBuf,

    /// Directory where transcript artifacts will be written.
    #[arg(short, long)]
    pub output: Option<PathBuf>,

    /// Replace an existing transcript.json in the output directory.
    #[arg(long)]
    pub force: bool,
}

#[derive(Debug, Args)]
pub struct SummarizeArgs {
    /// Existing transcript JSON to summarize.
    pub input: PathBuf,

    /// Replace an existing summary.md beside the transcript.
    #[arg(long)]
    pub force: bool,
}

#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    /// Create the default configuration file without overwriting an existing file.
    Init,
    /// Print the effective configuration path.
    Path,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn record_pipeline_flags_are_explicit() {
        let cli = Cli::try_parse_from(["meetlite", "record", "--summarize"]).unwrap();
        let Command::Record(args) = cli.command else {
            panic!("expected record command")
        };
        assert!(args.summarize);
        assert!(!args.transcribe);
    }

    #[test]
    fn transcribe_requires_an_existing_input() {
        assert!(Cli::try_parse_from(["meetlite", "transcribe"]).is_err());
    }

    #[test]
    fn summarize_requires_an_existing_input() {
        assert!(Cli::try_parse_from(["meetlite", "summarize"]).is_err());
    }
}
