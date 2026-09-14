use std::{
    io::{self, Write},
    path::Path,
    thread,
    time::Duration,
};

use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait};
use crossbeam_channel::bounded;

use crate::{
    cli::{
        CaptureArgs, Cli, Command, ConfigApiStyle, ConfigCommand, ConfigProvider, ConfigSetupArgs,
        RecordArgs, SummarizeArgs, TranscribeArgs,
    },
    config::{
        default_llm_config, default_stt_config, store_bearer_auth, ApiStyle, Config,
        LLM_API_KEY_ENV, STT_API_KEY_ENV,
    },
    credentials::Credentials,
    live_control::LiveControl,
    output::Output,
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
        Command::Config {
            command: ConfigCommand::Setup(args),
        } => setup_config(args, config.as_deref())?,
        Command::Devices => list_devices()?,
        Command::Start(args) => run_start(args, config.as_deref(), json)?,
        Command::Record(args) => run_record(args, config.as_deref(), json)?,
        Command::Transcribe(args) => run_transcribe(args, config.as_deref(), json)?,
        Command::Summarize(args) => run_summarize(args, config.as_deref(), json)?,
    }
    Ok(())
}

fn run_start(args: CaptureArgs, config_path: Option<&Path>, json: bool) -> Result<()> {
    run_live_pipeline(args, config_path, Output::new(json), true)
}

fn run_record(args: RecordArgs, config_path: Option<&Path>, json: bool) -> Result<()> {
    let RecordArgs {
        capture,
        transcribe,
        summarize,
    } = args;
    if transcribe || summarize {
        run_live_pipeline(capture, config_path, Output::new(json), summarize)
    } else {
        let config = Config::load_if_present(config_path)?;
        recording::record(capture, config.as_ref().map(|config| &config.recording))
    }
}

fn run_live_pipeline(
    args: CaptureArgs,
    config_path: Option<&Path>,
    output: Output,
    summarize_after: bool,
) -> Result<()> {
    let force = args.force;
    let config = Config::load(config_path)?;
    let stt = config.stt()?.clone();
    let control = LiveControl::install()?;
    if summarize_after {
        let llm = config.llm.as_ref().context(
            "no LLM provider is configured; add an `llm` section to the Meetlite configuration",
        )?;
        // Prompt for both credentials before capture begins.
        Credentials::for_llm(&llm.auth)?;
    }
    let transcription = transcription::transcribe_live(
        args,
        Some(&config.recording),
        stt,
        output,
        control.clone(),
    )?;
    if summarize_after {
        control.begin_summary();
        if control.summary_stopped() {
            return Ok(());
        }
        output.instruction("Summarizing. Press Ctrl-C to quit. Press Ctrl-D to quit immediately.");
        let transcript_path = transcription.transcript_path;
        let llm = config.llm.clone();
        let (sender, receiver) = bounded(1);
        thread::spawn(move || {
            let result = summary::summarize(&transcript_path, llm.as_ref(), force, output);
            let _ = sender.send(result);
        });
        loop {
            if control.summary_stopped() {
                output.instruction("Summary stopped.");
                return Ok(());
            }
            match receiver.recv_timeout(Duration::from_millis(50)) {
                Ok(result) => {
                    result?;
                    return Ok(());
                }
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                    anyhow::bail!("summary worker exited without a result")
                }
            }
        }
    }
    Ok(())
}

fn run_transcribe(args: TranscribeArgs, config_path: Option<&Path>, json: bool) -> Result<()> {
    let config = Config::load(config_path)?;
    transcription::transcribe_file(
        &args.input,
        args.output.as_deref(),
        config.stt()?,
        args.force,
        Output::new(json),
    )?;
    Ok(())
}

fn run_summarize(args: SummarizeArgs, config_path: Option<&Path>, json: bool) -> Result<()> {
    let config = Config::load(config_path)?;
    summary::summarize(
        &args.input,
        config.llm.as_ref(),
        args.force,
        Output::new(json),
    )?;
    Ok(())
}

fn setup_config(args: ConfigSetupArgs, config_path: Option<&Path>) -> Result<()> {
    let provider = args.provider;
    let mut config = Config::load_or_default(config_path)?;
    let token = match args.api_key {
        Some(api_key) => api_key,
        None => prompt_secret("API key (leave empty for none): ")?,
    };
    let (auth, warning) = store_bearer_auth(provider.name(), token, !args.no_keyring);

    match provider {
        ConfigProvider::Stt => {
            let mut stt = config.stt.unwrap_or_else(default_stt_config);
            stt.api_style = prompt_api_style(args.api_style, "STT API style", stt.api_style)?;
            stt.transcription_path = stt.api_style.default_stt_path().to_owned();
            stt.base_url = prompt_arg_or_default(args.base_url, "STT base URL", &stt.base_url)?;
            stt.model = prompt_arg_or_default(args.model, "STT model", &stt.model)?;
            stt.response_format = prompt_arg_or_default(
                args.response_format,
                "STT response format",
                &stt.response_format,
            )?;
            stt.language = match args.language {
                Some(language) if language.is_empty() => None,
                Some(language) => Some(language),
                None => prompt_optional("STT language hint", stt.language.as_deref())?,
            };
            stt.prompt = match args.prompt {
                Some(prompt) if prompt.is_empty() => None,
                Some(prompt) => Some(prompt),
                None => prompt_optional("STT prompt", stt.prompt.as_deref())?,
            };
            stt.auth = auth;
            config.stt = Some(stt);
        }
        ConfigProvider::Llm => {
            let mut llm = config.llm.unwrap_or_else(default_llm_config);
            llm.api_style = prompt_api_style(args.api_style, "LLM API style", llm.api_style)?;
            llm.chat_completions_path = llm.api_style.default_llm_path().to_owned();
            llm.base_url = prompt_arg_or_default(args.base_url, "LLM base URL", &llm.base_url)?;
            llm.model = prompt_arg_or_default(args.model, "LLM model", &llm.model)?;
            llm.instructions = match args.instructions {
                Some(instructions) if instructions.is_empty() => None,
                Some(instructions) => Some(instructions),
                None => prompt_optional("Summary instructions", llm.instructions.as_deref())?,
            };
            llm.auth = auth;
            config.llm = Some(llm);
        }
    }

    let path = config.save(config_path)?;
    if let Some(warning) = warning {
        eprintln!("Keyring unavailable; stored API key in private config: {warning}");
    }
    println!("Updated configuration at {}", path.display());
    println!(
        "{} overrides this API key when set.",
        provider.override_env()
    );
    Ok(())
}

fn prompt_arg_or_default(value: Option<String>, label: &str, default: &str) -> Result<String> {
    match value {
        Some(value) => Ok(value),
        None => prompt_default(label, default),
    }
}

fn prompt_api_style(
    value: Option<ConfigApiStyle>,
    label: &str,
    default: ApiStyle,
) -> Result<ApiStyle> {
    match value {
        Some(value) => Ok(value.into()),
        None => prompt_api_style_default(label, default),
    }
}

fn prompt_api_style_default(label: &str, default: ApiStyle) -> Result<ApiStyle> {
    let default_label = default.label();
    print!("{label} [{default_label}]: ");
    io::stdout().flush().context("could not flush stdout")?;
    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .context("could not read setup input")?;
    let value = input.trim();
    if value.is_empty() || value == default_label {
        Ok(default)
    } else {
        anyhow::bail!("unsupported API style {value:?}; supported style: {default_label}")
    }
}

fn prompt_default(label: &str, default: &str) -> Result<String> {
    print!("{label} [{default}]: ");
    io::stdout().flush().context("could not flush stdout")?;
    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .context("could not read setup input")?;
    let value = input.trim().to_owned();
    if value.is_empty() {
        Ok(default.to_owned())
    } else {
        Ok(value)
    }
}

fn prompt_optional(label: &str, default: Option<&str>) -> Result<Option<String>> {
    let prompt = match default {
        Some(default) => format!("{label} [{default}]: "),
        None => format!("{label} (optional): "),
    };
    print!("{prompt}");
    io::stdout().flush().context("could not flush stdout")?;
    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .context("could not read setup input")?;
    let value = input.trim().to_owned();
    if value.is_empty() {
        Ok(default.map(str::to_owned))
    } else {
        Ok(Some(value))
    }
}

fn prompt_secret(label: &str) -> Result<String> {
    rpassword::prompt_password(label).context("could not read API key")
}

impl From<ConfigApiStyle> for ApiStyle {
    fn from(value: ConfigApiStyle) -> Self {
        match value {
            ConfigApiStyle::OpenAiCompatible => ApiStyle::OpenAiCompatible,
        }
    }
}

impl ApiStyle {
    fn label(self) -> &'static str {
        match self {
            ApiStyle::OpenAiCompatible => "openai-compatible",
        }
    }

    fn default_stt_path(self) -> &'static str {
        match self {
            ApiStyle::OpenAiCompatible => "/audio/transcriptions",
        }
    }

    fn default_llm_path(self) -> &'static str {
        match self {
            ApiStyle::OpenAiCompatible => "/chat/completions",
        }
    }
}

impl ConfigProvider {
    fn name(self) -> &'static str {
        match self {
            ConfigProvider::Stt => "stt",
            ConfigProvider::Llm => "llm",
        }
    }

    fn override_env(self) -> &'static str {
        match self {
            ConfigProvider::Stt => STT_API_KEY_ENV,
            ConfigProvider::Llm => LLM_API_KEY_ENV,
        }
    }
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
