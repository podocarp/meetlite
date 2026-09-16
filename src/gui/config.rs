use std::{
    env,
    io::{self, BufRead, BufReader, Write},
    path::PathBuf,
    process::{Child, Command, ExitStatus, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender, TryRecvError, TrySendError},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};

use super::process::resolve_cli;

pub(crate) const SAVED_API_KEY_MASK: &str = "********";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub(crate) struct ProviderStatus {
    pub(crate) base_url: String,
    pub(crate) model: String,
    #[serde(default)]
    pub(crate) prompt: Option<String>,
    pub(crate) credential_configured: bool,
    #[serde(default)]
    pub(crate) managed_credential_configured: Option<bool>,
    #[serde(default)]
    pub(crate) auth_source: String,
    #[serde(default)]
    pub(crate) auth_provenance: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub(crate) struct LlmStatus {
    pub(crate) base_url: String,
    pub(crate) model: String,
    pub(crate) credential_configured: bool,
    #[serde(default)]
    pub(crate) managed_credential_configured: Option<bool>,
    #[serde(default)]
    pub(crate) auth_source: String,
    #[serde(default)]
    pub(crate) auth_provenance: String,
    #[serde(default)]
    pub(crate) instructions: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub(crate) struct ConfigStatus {
    pub(crate) path: PathBuf,
    pub(crate) usable: bool,
    #[serde(default)]
    pub(crate) reason: Option<String>,
    pub(crate) summary_enabled: bool,
    pub(crate) stt: ProviderStatus,
    pub(crate) llm: LlmStatus,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum ConfigEvent {
    ConfigStatus {
        path: PathBuf,
        usable: bool,
        #[serde(default)]
        reason: Option<String>,
        summary_enabled: bool,
        stt: ProviderStatus,
        llm: LlmStatus,
    },
    ConfigSaved {
        path: PathBuf,
        #[serde(default)]
        stored_credentials: Vec<String>,
    },
    Error {
        message: String,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ConfigState {
    Checking { onboarding: bool },
    Main,
    Editing { onboarding: bool },
    Saving { onboarding: bool },
    ProbeError { onboarding: bool },
    ApplyError { onboarding: bool },
}

impl Default for ConfigState {
    fn default() -> Self {
        Self::Checking { onboarding: true }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ConfigAction {
    StatusUsable,
    StatusUnusable,
    OpenSettings,
    OpenOnboarding,
    Save,
    SaveSucceeded,
    Failed,
    Retry,
    UseExisting,
    Cancel,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ConfigEffect {
    Probe,
    Apply,
}

impl ConfigState {
    pub(crate) fn reduce(&mut self, action: ConfigAction) -> Option<ConfigEffect> {
        match (*self, action) {
            (Self::Checking { .. }, ConfigAction::StatusUsable) => {
                *self = Self::Main;
                None
            }
            (Self::Checking { .. }, ConfigAction::StatusUnusable) => {
                *self = Self::Editing { onboarding: true };
                None
            }
            (Self::Main, ConfigAction::OpenSettings) => {
                *self = Self::Editing { onboarding: false };
                None
            }
            (_, ConfigAction::OpenOnboarding) => {
                *self = Self::Editing { onboarding: true };
                None
            }
            (Self::Editing { onboarding }, ConfigAction::Save)
            | (Self::ApplyError { onboarding }, ConfigAction::Save) => {
                *self = Self::Saving { onboarding };
                Some(ConfigEffect::Apply)
            }
            (Self::Saving { onboarding }, ConfigAction::SaveSucceeded) => {
                *self = Self::Checking { onboarding };
                Some(ConfigEffect::Probe)
            }
            (Self::Checking { onboarding }, ConfigAction::Failed) => {
                *self = Self::ProbeError { onboarding };
                None
            }
            (Self::Saving { onboarding }, ConfigAction::Failed) => {
                *self = Self::ApplyError { onboarding };
                None
            }
            (Self::ProbeError { onboarding }, ConfigAction::Retry)
            | (Self::ProbeError { onboarding }, ConfigAction::UseExisting)
            | (Self::ApplyError { onboarding }, ConfigAction::UseExisting) => {
                *self = Self::Checking { onboarding };
                Some(ConfigEffect::Probe)
            }
            (Self::ApplyError { onboarding }, ConfigAction::Retry) => {
                *self = Self::Saving { onboarding };
                Some(ConfigEffect::Apply)
            }
            (Self::Editing { onboarding: false }, ConfigAction::Cancel)
            | (Self::ProbeError { onboarding: false }, ConfigAction::Cancel)
            | (Self::ApplyError { onboarding: false }, ConfigAction::Cancel) => {
                *self = Self::Main;
                None
            }
            _ => None,
        }
    }

    pub(crate) fn onboarding(self) -> bool {
        match self {
            Self::Checking { onboarding }
            | Self::Editing { onboarding }
            | Self::Saving { onboarding }
            | Self::ProbeError { onboarding }
            | Self::ApplyError { onboarding } => onboarding,
            Self::Main => false,
        }
    }

    pub(crate) fn form_visible(self) -> bool {
        matches!(
            self,
            Self::Editing { .. } | Self::Saving { .. } | Self::ApplyError { .. }
        )
    }

    pub(crate) fn error_visible(self) -> bool {
        matches!(self, Self::ProbeError { .. })
    }
}

pub(crate) struct ConfigForm {
    pub(crate) stt_base_url: String,
    pub(crate) stt_model: String,
    pub(crate) stt_prompt: String,
    pub(crate) stt_api_key: String,
    pub(crate) show_stt_api_key: bool,
    pub(crate) stt_managed_credential: bool,
    pub(crate) stt_auth_source: String,
    pub(crate) summary_enabled: bool,
    pub(crate) llm_base_url: String,
    pub(crate) llm_model: String,
    pub(crate) llm_api_key: String,
    pub(crate) show_llm_api_key: bool,
    pub(crate) llm_managed_credential: bool,
    pub(crate) llm_auth_source: String,
    pub(crate) instructions: String,
}

impl Default for ConfigForm {
    fn default() -> Self {
        Self {
            stt_base_url: "https://api.openai.com/v1".into(),
            stt_model: "whisper-1".into(),
            stt_prompt: String::new(),
            stt_api_key: String::new(),
            show_stt_api_key: false,
            stt_managed_credential: false,
            stt_auth_source: "none".into(),
            summary_enabled: true,
            llm_base_url: "https://api.openai.com/v1".into(),
            llm_model: "gpt-4o-mini".into(),
            llm_api_key: String::new(),
            show_llm_api_key: false,
            llm_managed_credential: false,
            llm_auth_source: "none".into(),
            instructions: String::new(),
        }
    }
}

impl ConfigForm {
    pub(crate) fn from_status(status: &ConfigStatus) -> Self {
        let stt_managed_credential = status.stt.auth_source == "managed_keyring"
            && status.stt.managed_credential_configured == Some(true);
        let llm_managed_credential = status.llm.auth_source == "managed_keyring"
            && status.llm.managed_credential_configured == Some(true);
        Self {
            stt_base_url: status.stt.base_url.clone(),
            stt_model: status.stt.model.clone(),
            stt_prompt: status.stt.prompt.clone().unwrap_or_default(),
            stt_api_key: if stt_managed_credential {
                SAVED_API_KEY_MASK.into()
            } else {
                String::new()
            },
            show_stt_api_key: false,
            stt_managed_credential,
            stt_auth_source: status.stt.auth_source.clone(),
            summary_enabled: status.summary_enabled,
            llm_base_url: status.llm.base_url.clone(),
            llm_model: status.llm.model.clone(),
            llm_api_key: if llm_managed_credential {
                SAVED_API_KEY_MASK.into()
            } else {
                String::new()
            },
            show_llm_api_key: false,
            llm_managed_credential,
            llm_auth_source: status.llm.auth_source.clone(),
            instructions: status.llm.instructions.clone().unwrap_or_default(),
        }
    }

    pub(crate) fn payload(&self) -> ApplyPayload<'_> {
        let (stt_api_key, remove_stt_api_key) =
            key_change(&self.stt_api_key, self.stt_managed_credential);
        let (llm_api_key, remove_llm_api_key) =
            key_change(&self.llm_api_key, self.llm_managed_credential);
        ApplyPayload {
            stt: ProviderApply {
                base_url: self.stt_base_url.trim(),
                model: self.stt_model.trim(),
                prompt: nonempty(&self.stt_prompt),
                api_key: stt_api_key,
                remove_saved_key: remove_stt_api_key,
            },
            summary: SummaryApply {
                enabled: self.summary_enabled,
                llm: LlmApply {
                    base_url: self.llm_base_url.trim(),
                    model: self.llm_model.trim(),
                    api_key: llm_api_key,
                    remove_saved_key: remove_llm_api_key,
                    instructions: nonempty(&self.instructions),
                },
            },
        }
    }

    pub(crate) fn reconcile_key_visibility(&mut self) {
        if self.stt_api_key.is_empty() || self.stt_api_key == SAVED_API_KEY_MASK {
            self.show_stt_api_key = false;
        }
        if self.llm_api_key.is_empty() || self.llm_api_key == SAVED_API_KEY_MASK {
            self.show_llm_api_key = false;
        }
    }
}

fn key_change(value: &str, managed_credential: bool) -> (Option<&str>, bool) {
    let value = value.trim();
    if managed_credential && value == SAVED_API_KEY_MASK {
        (None, false)
    } else if value.is_empty() {
        (None, managed_credential)
    } else {
        (Some(value), false)
    }
}

fn nonempty(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty()).then_some(value)
}

#[derive(Serialize)]
pub(crate) struct ApplyPayload<'a> {
    stt: ProviderApply<'a>,
    summary: SummaryApply<'a>,
}

#[derive(Serialize)]
struct SummaryApply<'a> {
    enabled: bool,
    llm: LlmApply<'a>,
}

#[derive(Serialize)]
struct ProviderApply<'a> {
    base_url: &'a str,
    model: &'a str,
    prompt: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    api_key: Option<&'a str>,
    remove_saved_key: bool,
}

#[derive(Serialize)]
struct LlmApply<'a> {
    base_url: &'a str,
    model: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    api_key: Option<&'a str>,
    remove_saved_key: bool,
    instructions: Option<&'a str>,
}

#[derive(Debug)]
pub(crate) enum ConfigProcessEvent {
    Stdout(ConfigEvent),
    ProtocolError(String),
    Stderr(String),
    TerminationDelivered,
    TerminationFailed(String),
    Exited(ExitStatus),
    WaitFailed(String),
    InputFailed(String),
}

#[derive(Debug)]
pub(crate) struct ConfigChildController {
    commands: Sender<ConfigProcessCommand>,
}

impl ConfigChildController {
    pub(crate) fn terminate(&self) -> io::Result<()> {
        self.commands
            .send(ConfigProcessCommand::Terminate)
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "Meetlite configuration command has exited",
                )
            })
    }
}

const EXIT_DRAIN_GRACE: Duration = Duration::from_millis(250);
const MAX_PROTOCOL_LINE_BYTES: usize = 2 * 1024 * 1024;
const MAX_DIAGNOSTIC_LINE_BYTES: usize = 64 * 1024;
const PROCESS_CHANNEL_CAPACITY: usize = 64;
const PROTOCOL_TRUNCATED: &str =
    "Meetlite configuration protocol output was truncated because a line exceeded 2 MiB";
const DIAGNOSTIC_TRUNCATED: &str =
    "Meetlite configuration diagnostic output was truncated because a line exceeded 64 KiB";
const OUTPUT_DROPPED: &str =
    "Meetlite configuration output was truncated because the display queue was full";

impl ConfigEvent {
    pub(crate) fn into_status(self) -> Option<ConfigStatus> {
        match self {
            Self::ConfigStatus {
                path,
                usable,
                reason,
                summary_enabled,
                stt,
                llm,
            } => Some(ConfigStatus {
                path,
                usable,
                reason,
                summary_enabled,
                stt,
                llm,
            }),
            _ => None,
        }
    }
}

pub(crate) fn parse_line(line: &str) -> Result<ConfigEvent, String> {
    serde_json::from_str(line).map_err(|error| format!("Invalid configuration event: {error}"))
}

pub(crate) fn spawn_status() -> io::Result<(ConfigChildController, Receiver<ConfigProcessEvent>)> {
    spawn_command(status_command(resolve_cli()?), None)
}

pub(crate) fn spawn_apply_input(
    input: Vec<u8>,
) -> io::Result<(ConfigChildController, Receiver<ConfigProcessEvent>)> {
    spawn_command(apply_command(resolve_cli()?), Some(input))
}

fn status_command(executable: PathBuf) -> Command {
    let mut command = Command::new(executable);
    command.args(["--json", "config", "status"]);
    set_launch_directory(&mut command);
    command
}

fn apply_command(executable: PathBuf) -> Command {
    let mut command = Command::new(executable);
    command.args(["--json", "config", "apply", "--stdin-json"]);
    set_launch_directory(&mut command);
    command
}

fn set_launch_directory(command: &mut Command) {
    let directory = env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_dir())
        .or_else(|| env::current_dir().ok().filter(|path| path.is_dir()));
    if let Some(directory) = directory {
        command.current_dir(directory);
    }
}

fn spawn_command(
    mut command: Command,
    input: Option<Vec<u8>>,
) -> io::Result<(ConfigChildController, Receiver<ConfigProcessEvent>)> {
    command
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("configuration child stdout was not piped"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("configuration child stderr was not piped"))?;
    let stdin = child.stdin.take();
    let (internal_sender, internal_receiver) = mpsc::sync_channel(PROCESS_CHANNEL_CAPACITY);
    let (event_sender, event_receiver) = mpsc::sync_channel(PROCESS_CHANNEL_CAPACITY);
    let (command_sender, command_receiver) = mpsc::channel();
    let output_dropped = Arc::new(AtomicBool::new(false));

    spawn_stdout_reader(stdout, internal_sender.clone(), Arc::clone(&output_dropped));
    spawn_stderr_reader(stderr, internal_sender.clone(), Arc::clone(&output_dropped));
    if let Some(input) = input {
        spawn_input_writer(stdin, input, internal_sender);
    }
    thread::spawn(move || {
        supervise(
            child,
            internal_receiver,
            command_receiver,
            event_sender,
            output_dropped,
        )
    });

    Ok((
        ConfigChildController {
            commands: command_sender,
        },
        event_receiver,
    ))
}

fn spawn_input_writer(
    stdin: Option<impl Write + Send + 'static>,
    input: Vec<u8>,
    sender: SyncSender<ConfigInternalEvent>,
) {
    thread::spawn(move || {
        let result = stdin
            .ok_or_else(|| io::Error::other("configuration child stdin was not piped"))
            .and_then(|mut stdin| stdin.write_all(&input));
        if let Err(error) = result {
            let _ = sender.send(ConfigInternalEvent::Output(
                ConfigProcessEvent::InputFailed(format!(
                    "Could not write configuration to Meetlite CLI: {error}"
                )),
            ));
        }
    });
}

fn read_bounded_line(
    reader: &mut impl BufRead,
    max_bytes: usize,
) -> io::Result<Option<Result<String, ()>>> {
    let mut bytes = Vec::new();
    let mut truncated = false;
    loop {
        let buffer = reader.fill_buf()?;
        if buffer.is_empty() {
            if bytes.is_empty() && !truncated {
                return Ok(None);
            }
            break;
        }
        let newline = buffer.iter().position(|byte| *byte == b'\n');
        let take = newline.unwrap_or(buffer.len());
        let remaining = max_bytes.saturating_sub(bytes.len());
        bytes.extend_from_slice(&buffer[..take.min(remaining)]);
        truncated |= take > remaining;
        reader.consume(take + usize::from(newline.is_some()));
        if newline.is_some() {
            break;
        }
    }
    if truncated {
        return Ok(Some(Err(())));
    }
    if bytes.last() == Some(&b'\r') {
        bytes.pop();
    }
    String::from_utf8(bytes)
        .map(Ok)
        .map(Some)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "output was not valid UTF-8"))
}

fn send_internal(
    sender: &SyncSender<ConfigInternalEvent>,
    event: ConfigInternalEvent,
    output_dropped: &AtomicBool,
    preserve: bool,
) -> bool {
    match sender.try_send(event) {
        Ok(()) => true,
        Err(TrySendError::Full(event)) if preserve => sender.send(event).is_ok(),
        Err(TrySendError::Full(_)) => {
            output_dropped.store(true, Ordering::Release);
            true
        }
        Err(TrySendError::Disconnected(_)) => false,
    }
}

fn spawn_stdout_reader(
    stdout: impl io::Read + Send + 'static,
    sender: SyncSender<ConfigInternalEvent>,
    output_dropped: Arc<AtomicBool>,
) {
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        loop {
            let event = match read_bounded_line(&mut reader, MAX_PROTOCOL_LINE_BYTES) {
                Ok(Some(Ok(line))) => match parse_line(&line) {
                    Ok(event) => ConfigProcessEvent::Stdout(event),
                    Err(error) => ConfigProcessEvent::ProtocolError(error),
                },
                Ok(Some(Err(()))) => ConfigProcessEvent::ProtocolError(PROTOCOL_TRUNCATED.into()),
                Ok(None) => break,
                Err(error) => {
                    let event = ConfigProcessEvent::ProtocolError(format!(
                        "Could not read configuration output: {error}"
                    ));
                    let _ = send_internal(
                        &sender,
                        ConfigInternalEvent::Output(event),
                        &output_dropped,
                        true,
                    );
                    break;
                }
            };
            if !send_internal(
                &sender,
                ConfigInternalEvent::Output(event),
                &output_dropped,
                true,
            ) {
                return;
            }
        }
        let _ = sender.send(ConfigInternalEvent::StdoutClosed);
    });
}

fn spawn_stderr_reader(
    stderr: impl io::Read + Send + 'static,
    sender: SyncSender<ConfigInternalEvent>,
    output_dropped: Arc<AtomicBool>,
) {
    thread::spawn(move || {
        let mut reader = BufReader::new(stderr);
        loop {
            let event = match read_bounded_line(&mut reader, MAX_DIAGNOSTIC_LINE_BYTES) {
                Ok(Some(Ok(line))) => ConfigProcessEvent::Stderr(line),
                Ok(Some(Err(()))) => ConfigProcessEvent::Stderr(DIAGNOSTIC_TRUNCATED.into()),
                Ok(None) => break,
                Err(error) => {
                    let event = ConfigProcessEvent::Stderr(format!(
                        "Could not read configuration diagnostics: {error}"
                    ));
                    let _ = send_internal(
                        &sender,
                        ConfigInternalEvent::Output(event),
                        &output_dropped,
                        false,
                    );
                    break;
                }
            };
            if !send_internal(
                &sender,
                ConfigInternalEvent::Output(event),
                &output_dropped,
                false,
            ) {
                return;
            }
        }
        let _ = sender.send(ConfigInternalEvent::StderrClosed);
    });
}

fn forward_event(
    events: &SyncSender<ConfigProcessEvent>,
    event: ConfigProcessEvent,
    output_dropped: &AtomicBool,
    preserve: bool,
) -> bool {
    match events.try_send(event) {
        Ok(()) => true,
        Err(TrySendError::Full(event)) if preserve => events.send(event).is_ok(),
        Err(TrySendError::Full(_)) => {
            output_dropped.store(true, Ordering::Release);
            true
        }
        Err(TrySendError::Disconnected(_)) => false,
    }
}

fn forward_drop_notice(
    events: &SyncSender<ConfigProcessEvent>,
    output_dropped: &AtomicBool,
    force: bool,
) {
    if !output_dropped.swap(false, Ordering::AcqRel) {
        return;
    }
    let event = ConfigProcessEvent::Stderr(OUTPUT_DROPPED.into());
    let sent = if force {
        events.send(event).is_ok()
    } else {
        events.try_send(event).is_ok()
    };
    if !sent && !force {
        output_dropped.store(true, Ordering::Release);
    }
}

enum ConfigProcessCommand {
    Terminate,
}

enum ConfigInternalEvent {
    Output(ConfigProcessEvent),
    StdoutClosed,
    StderrClosed,
}

fn supervise(
    mut child: Child,
    output: Receiver<ConfigInternalEvent>,
    commands: Receiver<ConfigProcessCommand>,
    events: SyncSender<ConfigProcessEvent>,
    output_dropped: Arc<AtomicBool>,
) {
    let mut stdout_closed = false;
    let mut stderr_closed = false;
    let mut exit = None;
    let mut exit_observed_at = None;
    let mut controls_connected = true;

    loop {
        loop {
            match commands.try_recv() {
                Ok(ConfigProcessCommand::Terminate) => {
                    let event = match child.kill() {
                        Ok(()) => ConfigProcessEvent::TerminationDelivered,
                        Err(error) => ConfigProcessEvent::TerminationFailed(error.to_string()),
                    };
                    let _ = events.send(event);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    controls_connected = false;
                    if exit.is_none() {
                        let _ = child.kill();
                    }
                    break;
                }
            }
        }

        if exit.is_none() {
            match child.try_wait() {
                Ok(Some(status)) => {
                    exit = Some(Ok(status));
                    exit_observed_at = Some(Instant::now());
                }
                Ok(None) => {}
                Err(error) => {
                    exit = Some(Err(error.to_string()));
                    exit_observed_at = Some(Instant::now());
                }
            }
        }

        if exit.is_some()
            && ((stdout_closed && stderr_closed)
                || exit_observed_at.is_some_and(|at| at.elapsed() >= EXIT_DRAIN_GRACE))
        {
            forward_drop_notice(&events, &output_dropped, true);
            let event = match exit.take().unwrap() {
                Ok(status) => ConfigProcessEvent::Exited(status),
                Err(error) => ConfigProcessEvent::WaitFailed(error),
            };
            let _ = events.send(event);
            return;
        }

        let wait = exit_observed_at
            .map(|at| EXIT_DRAIN_GRACE.saturating_sub(at.elapsed()))
            .unwrap_or(Duration::from_millis(20))
            .min(Duration::from_millis(20));
        forward_drop_notice(&events, &output_dropped, false);
        match output.recv_timeout(wait) {
            Ok(ConfigInternalEvent::Output(event)) => {
                let preserve = !matches!(event, ConfigProcessEvent::Stderr(_));
                if !forward_event(&events, event, &output_dropped, preserve) {
                    let _ = child.kill();
                    let _ = child.wait();
                    return;
                }
            }
            Ok(ConfigInternalEvent::StdoutClosed) => stdout_closed = true,
            Ok(ConfigInternalEvent::StderrClosed) => stderr_closed = true,
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                stdout_closed = true;
                stderr_closed = true;
            }
        }

        if !controls_connected && exit.is_none() {
            exit = Some(child.wait().map_err(|error| error.to_string()));
            exit_observed_at = Some(Instant::now());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, process::Command, time::Duration};

    use serde_json::Value;
    use tempfile::tempdir;

    use super::*;

    fn status_json(usable: bool) -> String {
        serde_json::json!({
            "type": "config_status",
            "path": "/tmp/config.json",
            "usable": usable,
            "reason": if usable { Value::Null } else { Value::String("missing_config".into()) },
            "summary_enabled": false,
            "stt": {
                "base_url": "https://stt.example/v1",
                "model": "speech",
                "prompt": "Product names",
                "credential_configured": true,
                "managed_credential_configured": true,
                "auth_source": "managed_keyring",
                "auth_provenance": "managed"
            },
            "llm": {
                "base_url": "https://llm.example/v1",
                "model": "chat",
                "credential_configured": false,
                "managed_credential_configured": false,
                "auth_source": "managed_keyring",
                "auth_provenance": "managed",
                "instructions": "Brief"
            }
        })
        .to_string()
    }

    fn receive_exit(receiver: &Receiver<ConfigProcessEvent>) -> ExitStatus {
        loop {
            match receiver.recv_timeout(Duration::from_secs(5)).unwrap() {
                ConfigProcessEvent::Exited(status) => return status,
                ConfigProcessEvent::WaitFailed(error) => panic!("{error}"),
                _ => {}
            }
        }
    }

    #[test]
    fn parses_status_saved_and_error_events() {
        let status = parse_line(&status_json(false)).unwrap();
        assert!(matches!(
            status,
            ConfigEvent::ConfigStatus {
                usable: false,
                summary_enabled: false,
                ..
            }
        ));
        assert!(parse_line(
            r#"{"type":"config_saved","path":"/tmp/config.json","stored_credentials":["stt"]}"#
        )
        .is_ok());
        assert!(parse_line(r#"{"type":"error","message":"invalid model"}"#).is_ok());
    }

    #[test]
    fn rejects_malformed_unknown_and_incomplete_events_without_echoing_input() {
        for line in [
            "not-json-secret-value",
            r#"{"type":"unknown"}"#,
            r#"{"type":"config_status","path":"x"}"#,
        ] {
            let error = parse_line(line).unwrap_err();
            assert!(!error.contains(line));
        }
    }

    #[test]
    fn reducer_opens_onboarding_only_for_unusable_status() {
        let mut usable = ConfigState::default();
        usable.reduce(ConfigAction::StatusUsable);
        assert_eq!(usable, ConfigState::Main);

        let mut unusable = ConfigState::default();
        unusable.reduce(ConfigAction::StatusUnusable);
        assert_eq!(unusable, ConfigState::Editing { onboarding: true });
        unusable.reduce(ConfigAction::Cancel);
        assert_eq!(unusable, ConfigState::Editing { onboarding: true });
    }

    #[test]
    fn reducer_retries_failed_apply_and_reprobes_existing_configuration() {
        let mut retry = ConfigState::Editing { onboarding: false };
        retry.reduce(ConfigAction::Save);
        retry.reduce(ConfigAction::Failed);
        assert_eq!(retry, ConfigState::ApplyError { onboarding: false });
        assert_eq!(retry.reduce(ConfigAction::Retry), Some(ConfigEffect::Apply));
        assert_eq!(retry, ConfigState::Saving { onboarding: false });

        let mut existing = ConfigState::ApplyError { onboarding: true };
        assert_eq!(
            existing.reduce(ConfigAction::UseExisting),
            Some(ConfigEffect::Probe)
        );
        assert_eq!(existing, ConfigState::Checking { onboarding: true });
    }

    #[test]
    fn failed_probe_has_a_distinct_non_editing_state() {
        let mut state = ConfigState::default();
        state.reduce(ConfigAction::Failed);
        assert_eq!(state, ConfigState::ProbeError { onboarding: true });
        assert!(!state.form_visible());
        assert!(state.error_visible());
        assert_eq!(state.reduce(ConfigAction::Retry), Some(ConfigEffect::Probe));
    }

    #[test]
    fn reducer_rechecks_after_save_before_closing_form() {
        let mut state = ConfigState::Editing { onboarding: true };
        state.reduce(ConfigAction::Save);
        assert_eq!(
            state.reduce(ConfigAction::SaveSucceeded),
            Some(ConfigEffect::Probe)
        );
        assert_eq!(state, ConfigState::Checking { onboarding: true });
        state.reduce(ConfigAction::StatusUsable);
        assert_eq!(state, ConfigState::Main);
    }

    #[test]
    fn saved_key_mask_preserves_clear_removes_and_text_replaces() {
        let status = parse_line(&status_json(true))
            .unwrap()
            .into_status()
            .unwrap();
        let mut form = ConfigForm::from_status(&status);
        assert_eq!(form.stt_prompt, "Product names");
        assert_eq!(form.stt_api_key, SAVED_API_KEY_MASK);

        let preserved = serde_json::to_value(form.payload()).unwrap();
        assert!(preserved["stt"].get("api_key").is_none());
        assert_eq!(preserved["stt"]["remove_saved_key"], false);

        form.stt_api_key.clear();
        let removed = serde_json::to_value(form.payload()).unwrap();
        assert!(removed["stt"].get("api_key").is_none());
        assert_eq!(removed["stt"]["remove_saved_key"], true);

        form.stt_prompt = "Names: Meetlite, Nia Chen".into();
        form.stt_api_key = "replacement-stt-secret".into();
        form.llm_api_key = "replacement-llm-secret".into();
        let replaced = serde_json::to_value(form.payload()).unwrap();
        assert_eq!(replaced["stt"]["api_key"], "replacement-stt-secret");
        assert_eq!(replaced["stt"]["remove_saved_key"], false);
        assert_eq!(replaced["stt"]["prompt"], "Names: Meetlite, Nia Chen");
        assert_eq!(
            replaced["summary"]["llm"]["api_key"],
            "replacement-llm-secret"
        );
        assert_eq!(replaced["summary"]["llm"]["remove_saved_key"], false);
        assert_eq!(replaced["summary"]["llm"]["instructions"], "Brief");
    }

    #[test]
    fn builds_exact_status_and_apply_argv_without_secrets() {
        let status = status_command(PathBuf::from("/tmp/meetlite"));
        assert_eq!(status.get_program(), "/tmp/meetlite");
        assert_eq!(
            status.get_args().collect::<Vec<_>>(),
            ["--json", "config", "status"]
        );

        let apply = apply_command(PathBuf::from("/tmp/meetlite"));
        assert_eq!(apply.get_program(), "/tmp/meetlite");
        assert_eq!(
            apply.get_args().collect::<Vec<_>>(),
            ["--json", "config", "apply", "--stdin-json"]
        );
        assert!(!apply
            .get_args()
            .any(|argument| argument.to_string_lossy().contains("secret")));
    }

    #[test]
    fn oversized_output_line_is_discarded_without_echoing_and_next_line_is_parsed() {
        let secret = "oversized-config-secret";
        let mut bytes = secret
            .repeat(MAX_PROTOCOL_LINE_BYTES / secret.len() + 2)
            .into_bytes();
        bytes.push(b'\n');
        bytes.extend_from_slice(b"{\"type\":\"error\",\"message\":\"next\"}\n");
        let (sender, receiver) = mpsc::sync_channel(4);
        spawn_stdout_reader(
            io::Cursor::new(bytes),
            sender,
            Arc::new(AtomicBool::new(false)),
        );

        assert!(matches!(
            receiver.recv_timeout(Duration::from_secs(2)).unwrap(),
            ConfigInternalEvent::Output(ConfigProcessEvent::ProtocolError(message))
                if message == PROTOCOL_TRUNCATED && !message.contains(secret)
        ));
        assert!(matches!(
            receiver.recv_timeout(Duration::from_secs(2)).unwrap(),
            ConfigInternalEvent::Output(ConfigProcessEvent::Stdout(ConfigEvent::Error { message }))
                if message == "next"
        ));
    }

    #[test]
    fn status_child_receives_null_stdin() {
        let mut command = Command::new("/bin/sh");
        command.args([
            "-c",
            "if read value; then exit 9; fi; printf '%s\\n' '{\"type\":\"error\",\"message\":\"done\"}'",
        ]);
        let (_controller, receiver) = spawn_command(command, None).unwrap();
        assert!(receive_exit(&receiver).success());
    }

    #[test]
    fn controller_terminates_child_and_reports_confirmed_exit() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "sleep 30"]);
        let (controller, receiver) = spawn_command(command, None).unwrap();

        controller.terminate().unwrap();
        assert!(matches!(
            receiver.recv_timeout(Duration::from_secs(5)).unwrap(),
            ConfigProcessEvent::TerminationDelivered
        ));
        assert!(!receive_exit(&receiver).success());
    }

    #[test]
    fn exited_child_does_not_wait_indefinitely_for_inherited_pipes() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "{ sleep 30; } & exit 0"]);
        let started = Instant::now();
        let (_controller, receiver) = spawn_command(command, None).unwrap();

        assert!(receive_exit(&receiver).success());
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn apply_secret_is_only_in_stdin_and_writer_closes_with_eof() {
        let directory = tempdir().unwrap();
        let captured = directory.path().join("captured.json");
        let secret = "stdin-only-secret";
        let script = format!(
            "input=$(cat); printf '%s' \"$input\" > '{}'; printf '%s\\n' '{{\"type\":\"config_saved\",\"path\":\"saved\"}}'",
            captured.display()
        );
        assert!(!script.contains(secret));
        let mut command = Command::new("/bin/sh");
        command.args(["-c", &script]);
        assert!(!command
            .get_args()
            .any(|argument| argument.to_string_lossy().contains(secret)));

        let input = serde_json::json!({"api_key": secret}).to_string();
        let (_controller, receiver) = spawn_command(command, Some(input.into_bytes())).unwrap();
        assert!(receive_exit(&receiver).success());
        assert_eq!(
            fs::read_to_string(captured).unwrap(),
            format!(r#"{{"api_key":"{secret}"}}"#)
        );
    }
}
