use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(name = "meetlite", about = "A lightweight local meeting recorder")]
pub struct Cli {
    /// Override the configuration file path.
    #[arg(long, global = true, env = "MEETLITE_CONFIG")]
    pub config: Option<PathBuf>,

    /// Emit machine-readable progress as newline-delimited JSON.
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
    /// Record, transcribe live, and summarize when recording finishes.
    Start(CaptureArgs),
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
pub struct CaptureArgs {
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
}

#[derive(Debug, Clone, Args)]
pub struct RecordArgs {
    #[command(flatten)]
    pub capture: CaptureArgs,

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
    /// Configure a transcription or summary provider.
    Setup(ConfigSetupArgs),
}

#[derive(Debug, Args)]
pub struct ConfigSetupArgs {
    /// Provider section to configure.
    #[arg(value_enum)]
    pub provider: ConfigProvider,

    /// Provider base URL.
    #[arg(long)]
    pub base_url: Option<String>,

    /// Provider model name.
    #[arg(long)]
    pub model: Option<String>,

    /// API key. Omit to be prompted, or pass an empty string to configure without one.
    #[arg(long)]
    pub api_key: Option<String>,

    /// Store the API key directly in config instead of the OS keyring.
    #[arg(long)]
    pub no_keyring: bool,

    /// Provider API style.
    #[arg(long, value_enum)]
    pub api_style: Option<ConfigApiStyle>,

    /// STT language hint.
    #[arg(long)]
    pub language: Option<String>,

    /// STT prompt for terminology and transcription style.
    #[arg(long)]
    pub prompt: Option<String>,

    /// STT response format.
    #[arg(long)]
    pub response_format: Option<String>,

    /// Extra summary instructions.
    #[arg(long)]
    pub instructions: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ConfigProvider {
    Stt,
    Llm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ConfigApiStyle {
    #[value(name = "openai-compatible")]
    OpenAiCompatible,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn start_accepts_capture_arguments_without_pipeline_flags() {
        let cli = Cli::try_parse_from([
            "meetlite",
            "start",
            "--duration",
            "60",
            "--output",
            "team-sync",
        ])
        .unwrap();
        let Command::Start(args) = cli.command else {
            panic!("expected start command")
        };
        assert_eq!(args.duration, Some(60));
        assert_eq!(args.output, Some(PathBuf::from("team-sync")));
        assert!(Cli::try_parse_from(["meetlite", "start", "--transcribe"]).is_err());
        assert!(Cli::try_parse_from(["meetlite", "start", "--summarize"]).is_err());
    }

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

    #[test]
    fn setup_is_not_a_public_command() {
        assert!(Cli::try_parse_from(["meetlite", "setup"]).is_err());
    }

    #[test]
    fn config_setup_accepts_provider() {
        let cli = Cli::try_parse_from([
            "meetlite",
            "config",
            "setup",
            "stt",
            "--api-key",
            "",
            "--api-style",
            "openai-compatible",
        ])
        .unwrap();
        let Command::Config {
            command: ConfigCommand::Setup(args),
        } = cli.command
        else {
            panic!("expected config setup command")
        };
        assert_eq!(args.provider, ConfigProvider::Stt);
        assert_eq!(args.api_key.as_deref(), Some(""));
        assert_eq!(args.api_style, Some(ConfigApiStyle::OpenAiCompatible));
    }
}
