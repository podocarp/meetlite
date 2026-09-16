use std::{
    path::Path,
    sync::{
        mpsc::{self, Receiver, TryRecvError},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

use eframe::egui::{self, Color32, FontData, FontDefinitions, FontFamily, RichText, Sense};

use super::{
    config::{
        self, ConfigAction, ConfigChildController, ConfigEffect, ConfigEvent, ConfigForm,
        ConfigProcessEvent, ConfigState, ConfigStatus,
    },
    events::{CliEvent, LifecyclePhase},
    process::{self, ChildController, ProcessControl, ProcessEvent},
    session::{ControlVisual, Session, SessionEffect, SessionEvent, SessionState},
};

const WINDOW_WIDTH: f32 = 360.0;
const WINDOW_HEIGHT: f32 = 500.0;
const CANCELLATION_GRACE: Duration = Duration::from_secs(2);
const CLOSE_TERMINATION_RETRY: Duration = Duration::from_millis(250);
const CONFIG_CLOSE_GRACE: Duration = Duration::from_secs(2);
const CONTROL_SIZE: egui::Vec2 = egui::vec2(180.0, 70.0);
const MAX_NON_TRANSCRIPT_ENTRIES: usize = 256;
const MAX_NON_TRANSCRIPT_BYTES: usize = 512 * 1024;
const MAX_ENTRY_BYTES: usize = 64 * 1024;
const ENTRY_TRUNCATION_SUFFIX: &str = "\n[output truncated]";
const HISTORY_TRUNCATED: &str = "Earlier session output was removed to limit memory use";
const MAX_CREDENTIAL_NOTICES: usize = 8;

const CJK_CHINESE: u8 = 1;
const CJK_JAPANESE: u8 = 2;
const CJK_KOREAN: u8 = 4;
const CJK_ALL: u8 = CJK_CHINESE | CJK_JAPANESE | CJK_KOREAN;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FontCandidate {
    path: &'static str,
    index: u32,
    coverage: u8,
}

#[cfg(target_os = "macos")]
const PLATFORM_FONT_CANDIDATES: &[FontCandidate] = &[
    FontCandidate {
        path: "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
        index: 0,
        coverage: CJK_ALL,
    },
    FontCandidate {
        path: "/System/Library/Fonts/Hiragino Sans GB.ttc",
        index: 0,
        coverage: CJK_CHINESE,
    },
    FontCandidate {
        path: "/System/Library/Fonts/STHeiti Light.ttc",
        index: 0,
        coverage: CJK_CHINESE,
    },
    FontCandidate {
        path: "/System/Library/Fonts/ヒラギノ角ゴシック W3.ttc",
        index: 0,
        coverage: CJK_JAPANESE,
    },
    FontCandidate {
        path: "/System/Library/Fonts/AquaKana.ttc",
        index: 0,
        coverage: CJK_JAPANESE,
    },
    FontCandidate {
        path: "/System/Library/Fonts/AppleSDGothicNeo.ttc",
        index: 0,
        coverage: CJK_KOREAN,
    },
    FontCandidate {
        path: "/System/Library/Fonts/Supplemental/AppleGothic.ttf",
        index: 0,
        coverage: CJK_KOREAN,
    },
];

#[cfg(target_os = "linux")]
const PLATFORM_FONT_CANDIDATES: &[FontCandidate] = &[
    FontCandidate {
        path: "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        index: 0,
        coverage: CJK_ALL,
    },
    FontCandidate {
        path: "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
        index: 0,
        coverage: CJK_ALL,
    },
    FontCandidate {
        path: "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc",
        index: 0,
        coverage: CJK_ALL,
    },
    FontCandidate {
        path: "/usr/local/share/fonts/NotoSansCJK-Regular.ttc",
        index: 0,
        coverage: CJK_ALL,
    },
    FontCandidate {
        path: "/usr/local/share/fonts/noto/NotoSansCJK-Regular.ttc",
        index: 0,
        coverage: CJK_ALL,
    },
    FontCandidate {
        path: "/usr/share/fonts/google-noto-sans-cjk-fonts/NotoSansCJK-VF.ttc",
        index: 0,
        coverage: CJK_ALL,
    },
    FontCandidate {
        path: "/usr/share/fonts/opentype/noto/NotoSansCJKsc-Regular.otf",
        index: 0,
        coverage: CJK_CHINESE,
    },
    FontCandidate {
        path: "/usr/share/fonts/opentype/noto/NotoSansCJKjp-Regular.otf",
        index: 0,
        coverage: CJK_JAPANESE,
    },
    FontCandidate {
        path: "/usr/share/fonts/opentype/noto/NotoSansCJKkr-Regular.otf",
        index: 0,
        coverage: CJK_KOREAN,
    },
];

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
const PLATFORM_FONT_CANDIDATES: &[FontCandidate] = &[];

fn select_font_candidates<F>(candidates: &[FontCandidate], mut is_readable: F) -> Vec<FontCandidate>
where
    F: FnMut(&Path) -> bool,
{
    let mut missing = CJK_ALL;
    let mut selected = Vec::new();
    for candidate in candidates {
        if candidate.coverage & missing != 0 && is_readable(Path::new(candidate.path)) {
            selected.push(*candidate);
            missing &= !candidate.coverage;
            if missing == 0 {
                break;
            }
        }
    }
    selected
}

fn append_font_fallbacks(
    definitions: &mut FontDefinitions,
    fonts: impl IntoIterator<Item = (String, FontData)>,
) {
    for (name, data) in fonts {
        if definitions.font_data.contains_key(&name) {
            continue;
        }
        definitions.font_data.insert(name.clone(), Arc::new(data));
        for family in [FontFamily::Proportional, FontFamily::Monospace] {
            definitions
                .families
                .entry(family)
                .or_default()
                .push(name.clone());
        }
    }
}

fn install_platform_font_fallbacks(context: &egui::Context) {
    let candidates = select_font_candidates(PLATFORM_FONT_CANDIDATES, |path| {
        std::fs::File::open(path).is_ok()
    });
    let fonts = candidates.into_iter().filter_map(|candidate| {
        std::fs::read(candidate.path).ok().map(|bytes| {
            let mut data = FontData::from_owned(bytes);
            data.index = candidate.index;
            (format!("meetlite-cjk-{}", candidate.path), data)
        })
    });
    let mut definitions = FontDefinitions::default();
    append_font_fallbacks(&mut definitions, fonts);
    context.set_fonts(definitions);
}

pub(crate) fn run() -> eframe::Result {
    let viewport = egui::ViewportBuilder::default()
        .with_inner_size([WINDOW_WIDTH, WINDOW_HEIGHT])
        .with_min_inner_size([WINDOW_WIDTH, WINDOW_HEIGHT])
        .with_max_inner_size([WINDOW_WIDTH, WINDOW_HEIGHT])
        .with_resizable(false);
    eframe::run_native(
        "Meetlite",
        eframe::NativeOptions {
            renderer: eframe::Renderer::Glow,
            viewport,
            ..Default::default()
        },
        Box::new(|creation_context| {
            install_platform_font_fallbacks(&creation_context.egui_ctx);
            Ok(Box::new(MeetliteApp::default()))
        }),
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EntryKind {
    Transcript,
    Summary,
    Status,
    Warning,
    Error,
}

#[derive(Debug, Eq, PartialEq)]
struct Entry {
    kind: EntryKind,
    text: String,
}

fn spinner_shows_stop(state: SessionState, enabled: bool, hovered: bool) -> bool {
    enabled && hovered && matches!(state, SessionState::Processing | SessionState::Summarizing)
}

fn block_secret_copy_cut(ui: &mut egui::Ui, id: egui::Id) {
    if ui.memory(|memory| memory.has_focus(id)) {
        ui.input_mut(|input| {
            input
                .events
                .retain(|event| !matches!(event, egui::Event::Copy | egui::Event::Cut));
        });
    }
}

fn secret_input(ui: &mut egui::Ui, id_salt: &'static str, value: &mut String, visible: &mut bool) {
    if value.is_empty() {
        *visible = false;
    }
    let id = ui.make_persistent_id(id_salt);
    block_secret_copy_cut(ui, id);
    ui.add(
        egui::TextEdit::singleline(value)
            .id(id)
            .password(!*visible)
            .hint_text("Enter API key"),
    );
    if ui
        .add_enabled(
            !value.is_empty(),
            egui::Button::new(if *visible { "Hide" } else { "Show" }),
        )
        .clicked()
    {
        *visible = !*visible;
    }
}

fn truncate_entry_text(mut text: String) -> String {
    if text.len() <= MAX_ENTRY_BYTES {
        return text;
    }
    let mut end = MAX_ENTRY_BYTES - ENTRY_TRUNCATION_SUFFIX.len();
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    text.push_str(ENTRY_TRUNCATION_SUFFIX);
    text
}

fn transcript_text(entries: &[Entry]) -> Option<String> {
    let text = entries
        .iter()
        .filter(|entry| entry.kind == EntryKind::Transcript)
        .map(|entry| entry.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    (!text.is_empty()).then_some(text)
}

fn source_speaker(source: Option<&str>) -> &str {
    match source {
        Some("microphone") => "You",
        Some("system") => "Remote",
        _ => "Meeting",
    }
}

fn final_transcript_entries(transcript: &serde_json::Value) -> Vec<String> {
    let mut entries = transcript
        .get("segments")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|segment| {
            let start_seconds = segment.get("start_seconds")?.as_f64()?;
            let text = segment.get("text")?.as_str()?.trim();
            if text.is_empty() {
                return None;
            }
            let speaker = segment
                .get("speaker")
                .and_then(serde_json::Value::as_str)
                .filter(|speaker| !speaker.trim().is_empty())
                .unwrap_or("Meeting");
            Some(format!("[{start_seconds:>8.2}s] {speaker}: {text}"))
        })
        .collect::<Vec<_>>();
    if entries.is_empty() {
        if let Some(text) = transcript
            .get("text")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
        {
            entries.push(format!("[    0.00s] Meeting: {text}"));
        }
    }
    entries
}

#[derive(Default)]
pub(crate) struct MeetliteApp {
    session: Session,
    entries: Vec<Entry>,
    summary_entry: Option<usize>,
    launch: Option<Receiver<std::io::Result<(ChildController, Receiver<ProcessEvent>)>>>,
    process: Option<Receiver<ProcessEvent>>,
    controller: Option<ChildController>,
    cancellation_deadline: Option<Instant>,
    close_requested: bool,
    close_termination_pending: bool,
    close_termination_delivered: bool,
    close_termination_retry_at: Option<Instant>,
    config_state: ConfigState,
    config_form: ConfigForm,
    config_status: Option<ConfigStatus>,
    pending_config_status: Option<ConfigStatus>,
    config_launch:
        Option<Receiver<std::io::Result<(ConfigChildController, Receiver<ConfigProcessEvent>)>>>,
    config_process: Option<Receiver<ConfigProcessEvent>>,
    config_controller: Option<ConfigChildController>,
    config_close_deadline: Option<Instant>,
    config_close_termination_requested: bool,
    config_error: Option<String>,
    config_reason: Option<String>,
    config_started: bool,
    config_status_event: bool,
    config_success_event: bool,
    config_error_event: bool,
    credential_notices: Vec<String>,
}

impl MeetliteApp {
    fn start_config_probe(&mut self) {
        if self.config_launch.is_some() || self.config_process.is_some() {
            return;
        }
        self.config_error = None;
        self.pending_config_status = None;
        self.config_close_termination_requested = false;
        self.config_status_event = false;
        self.config_success_event = false;
        self.config_error_event = false;
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let _ = sender.send(config::spawn_status());
        });
        self.config_launch = Some(receiver);
    }

    fn start_config_apply(&mut self) {
        if self.config_launch.is_some() || self.config_process.is_some() {
            return;
        }
        let input = match serde_json::to_vec(&self.config_form.payload()) {
            Ok(input) => input,
            Err(_) => {
                self.config_error = Some("Could not prepare configuration".into());
                self.config_state.reduce(ConfigAction::Failed);
                return;
            }
        };
        self.config_error = None;
        self.pending_config_status = None;
        self.config_close_termination_requested = false;
        self.config_status_event = false;
        self.config_success_event = false;
        self.config_error_event = false;
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let _ = sender.send(config::spawn_apply_input(input));
        });
        self.config_launch = Some(receiver);
    }

    fn apply_config_effect(&mut self, effect: ConfigEffect) {
        match effect {
            ConfigEffect::Probe => self.start_config_probe(),
            ConfigEffect::Apply => self.start_config_apply(),
        }
    }

    fn open_settings(&mut self) {
        if self.configuration_available() {
            if let Some(status) = &self.config_status {
                self.config_form = ConfigForm::from_status(status);
            }
            self.config_state.reduce(ConfigAction::OpenSettings);
        }
    }

    fn configuration_available(&self) -> bool {
        self.launch.is_none()
            && self.process.is_none()
            && self.config_launch.is_none()
            && self.config_process.is_none()
            && matches!(
                self.session.state(),
                SessionState::Ready
                    | SessionState::Complete
                    | SessionState::Stopped
                    | SessionState::Failed
                    | SessionState::SetupRequired
            )
    }

    fn primary_control(&mut self) {
        if matches!(
            self.session.state(),
            SessionState::Complete | SessionState::Stopped | SessionState::Failed
        ) {
            if self.launch.is_none() && self.process.is_none() {
                self.session.reduce(SessionEvent::PrimaryControlPressed);
                self.entries.clear();
                self.summary_entry = None;
                self.controller = None;
                self.cancellation_deadline = None;
            }
            return;
        }
        if let Some(effect) = self.session.reduce(SessionEvent::PrimaryControlPressed) {
            self.apply_effect(effect);
        }
    }

    fn apply_effect(&mut self, effect: SessionEffect) {
        match effect {
            SessionEffect::OpenSetup => {
                self.config_state.reduce(ConfigAction::OpenOnboarding);
            }
            SessionEffect::SpawnChild => {
                self.push_entry(EntryKind::Status, "Starting Meetlite…".into());
                let (sender, receiver) = mpsc::channel();
                thread::spawn(move || {
                    let _ = sender.send(process::spawn_cli());
                });
                self.launch = Some(receiver);
            }
            SessionEffect::SendInterrupt => {
                let result = self
                    .controller
                    .as_ref()
                    .ok_or_else(|| {
                        std::io::Error::new(
                            std::io::ErrorKind::BrokenPipe,
                            "Meetlite CLI is not running",
                        )
                    })
                    .and_then(ProcessControl::interrupt);
                if let Err(error) = result {
                    self.session.reduce(SessionEvent::SignalDeliveryFailed);
                    self.push_entry(
                        EntryKind::Error,
                        format!("Could not request a stop from Meetlite CLI: {error}"),
                    );
                }
            }
            SessionEffect::StartCancellationGrace => {
                self.cancellation_deadline = Some(Instant::now() + CANCELLATION_GRACE);
            }
            SessionEffect::TerminateChild => {
                self.cancellation_deadline = None;
                let result = self
                    .controller
                    .as_ref()
                    .ok_or_else(|| {
                        std::io::Error::new(
                            std::io::ErrorKind::BrokenPipe,
                            "Meetlite CLI is not running",
                        )
                    })
                    .and_then(ProcessControl::terminate);
                if let Err(error) = result {
                    self.session.reduce(SessionEvent::TerminationFailed);
                    self.push_entry(
                        EntryKind::Error,
                        format!("Could not terminate Meetlite CLI: {error}"),
                    );
                    if self.close_requested {
                        self.close_termination_retry_at =
                            Some(Instant::now() + CLOSE_TERMINATION_RETRY);
                    }
                }
            }
        }
    }

    fn request_close_termination(&mut self) {
        if self.close_termination_pending || self.close_termination_delivered {
            return;
        }
        let Some(controller) = &self.controller else {
            return;
        };
        match controller.terminate() {
            Ok(()) => {
                self.close_termination_pending = true;
                self.close_termination_retry_at = None;
            }
            Err(error) => {
                self.close_termination_retry_at = Some(Instant::now() + CLOSE_TERMINATION_RETRY);
                self.push_entry(
                    EntryKind::Error,
                    format!("Could not terminate Meetlite CLI while closing: {error}"),
                );
            }
        }
    }

    fn request_config_close_termination(&mut self) {
        if self.config_close_termination_requested {
            return;
        }
        if let Some(controller) = &self.config_controller {
            self.config_close_termination_requested = true;
            if let Err(error) = controller.terminate() {
                self.config_error = Some(self.redact_config_message(format!(
                    "Could not terminate Meetlite configuration command while closing: {error}"
                )));
            }
        }
    }

    fn request_close(&mut self) {
        self.close_requested = true;
        self.config_close_deadline
            .get_or_insert_with(|| Instant::now() + CONFIG_CLOSE_GRACE);
        self.request_close_termination();
        self.request_config_close_termination();
    }

    fn ready_to_close(&self) -> bool {
        let config_finished = self.config_launch.is_none() && self.config_process.is_none();
        let config_fallback_elapsed = self
            .config_close_deadline
            .is_some_and(|deadline| Instant::now() >= deadline);
        self.close_requested
            && (config_finished || config_fallback_elapsed)
            && self.launch.is_none()
            && self.process.is_none()
    }

    fn drain_config_messages(&mut self) {
        let launch_result = self.config_launch.as_ref().map(Receiver::try_recv);
        match launch_result {
            Some(Ok(result)) => {
                self.config_launch = None;
                match result {
                    Ok((controller, receiver)) => {
                        self.config_controller = Some(controller);
                        self.config_process = Some(receiver);
                        if self.close_requested {
                            self.request_config_close_termination();
                        }
                    }
                    Err(error) => self.fail_configuration(format!(
                        "Could not launch Meetlite CLI for configuration: {error}"
                    )),
                }
            }
            Some(Err(TryRecvError::Disconnected)) => {
                self.config_launch = None;
                self.fail_configuration("Configuration launch failed unexpectedly".into());
            }
            Some(Err(TryRecvError::Empty)) | None => {}
        }

        let mut events = Vec::new();
        let mut disconnected = false;
        if let Some(receiver) = &self.config_process {
            loop {
                match receiver.try_recv() {
                    Ok(event) => events.push(event),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                }
            }
        }
        for event in events {
            self.handle_config_process_event(event);
        }
        if disconnected && self.config_process.is_some() {
            self.config_process = None;
            self.config_controller = None;
            self.fail_configuration("Configuration command disconnected unexpectedly".into());
        }
    }

    fn is_credential_notice(line: &str) -> bool {
        line.starts_with("Keychain access needed for Meetlite/Meetlite API Credentials.")
            || line.starts_with("macOS may prompt for Keychain access;")
    }

    fn remember_credential_notice(&mut self, line: String) {
        let line = self.redact_config_message(line);
        if !self.credential_notices.contains(&line) {
            if self.credential_notices.len() == MAX_CREDENTIAL_NOTICES {
                self.credential_notices.remove(0);
            }
            self.credential_notices.push(line.clone());
            self.push_entry(EntryKind::Status, line);
        }
    }

    fn redact_config_message(&self, mut message: String) -> String {
        for secret in [
            self.config_form.stt_api_key.trim(),
            self.config_form.llm_api_key.trim(),
        ] {
            if !secret.is_empty() {
                message = message.replace(secret, "[redacted]");
            }
        }
        message
    }

    fn fail_configuration(&mut self, message: String) {
        self.config_error = Some(self.redact_config_message(message));
        self.config_state.reduce(ConfigAction::Failed);
    }

    fn accept_config_save(&mut self) -> Option<ConfigEffect> {
        self.config_form.stt_api_key.clear();
        self.config_form.show_stt_api_key = false;
        self.config_form.llm_api_key.clear();
        self.config_form.show_llm_api_key = false;
        self.config_state.reduce(ConfigAction::SaveSucceeded)
    }

    fn handle_config_process_event(&mut self, event: ConfigProcessEvent) {
        match event {
            ConfigProcessEvent::Stdout(event) => match event {
                event @ ConfigEvent::ConfigStatus { .. } => {
                    let status = event.into_status().unwrap();
                    self.config_status_event = true;
                    self.pending_config_status = Some(status);
                }
                ConfigEvent::ConfigSaved { .. } => self.config_success_event = true,
                ConfigEvent::Error { message } => {
                    self.config_error_event = true;
                    self.config_error = Some(self.redact_config_message(message));
                }
            },
            ConfigProcessEvent::ProtocolError(error) | ConfigProcessEvent::InputFailed(error) => {
                self.config_error_event = true;
                self.config_error = Some(self.redact_config_message(error));
            }
            ConfigProcessEvent::Stderr(line) if Self::is_credential_notice(&line) => {
                self.remember_credential_notice(line);
            }
            ConfigProcessEvent::Stderr(line) => {
                if self.config_error.is_none() && !line.trim().is_empty() {
                    self.config_error = Some(self.redact_config_message(line));
                }
            }
            ConfigProcessEvent::TerminationDelivered => {}
            ConfigProcessEvent::TerminationFailed(error) => {
                self.config_error = Some(self.redact_config_message(format!(
                    "Could not terminate Meetlite configuration command: {error}"
                )));
            }
            ConfigProcessEvent::Exited(status) => {
                self.config_process = None;
                self.config_controller = None;
                if self.close_requested {
                    self.pending_config_status = None;
                    return;
                }
                let saving = matches!(self.config_state, ConfigState::Saving { .. });
                if status.success()
                    && ((saving && self.config_success_event && !self.config_error_event)
                        || (!saving && self.config_status_event && !self.config_error_event))
                {
                    if saving {
                        if let Some(effect) = self.accept_config_save() {
                            self.apply_config_effect(effect);
                        }
                    } else if let Some(status) = self.pending_config_status.take() {
                        self.config_reason = status
                            .reason
                            .clone()
                            .map(|reason| self.redact_config_message(reason));
                        self.config_form = ConfigForm::from_status(&status);
                        self.session.set_summary_enabled(status.summary_enabled);
                        self.config_status = Some(status.clone());
                        self.config_state.reduce(if status.usable {
                            ConfigAction::StatusUsable
                        } else {
                            ConfigAction::StatusUnusable
                        });
                        self.session.reduce(if status.usable {
                            SessionEvent::SetupCompleted
                        } else {
                            SessionEvent::SetupRequired
                        });
                    }
                } else {
                    self.pending_config_status = None;
                    let message = self.config_error.clone().unwrap_or_else(|| {
                        format!("Meetlite configuration command exited with {status}")
                    });
                    self.fail_configuration(message);
                }
            }
            ConfigProcessEvent::WaitFailed(error) => {
                self.config_process = None;
                self.config_controller = None;
                self.fail_configuration(format!(
                    "Could not wait for Meetlite configuration command: {error}"
                ));
            }
        }
    }

    fn drain_messages(&mut self, context: &egui::Context) {
        self.drain_config_messages();
        let launch_result = self.launch.as_ref().map(Receiver::try_recv);
        match launch_result {
            Some(Ok(result)) => {
                self.launch = None;
                match result {
                    Ok((controller, receiver)) => {
                        self.controller = Some(controller);
                        self.process = Some(receiver);
                        if self.close_requested {
                            self.request_close_termination();
                        }
                    }
                    Err(error) => {
                        self.push_entry(
                            EntryKind::Error,
                            format!("Could not launch Meetlite CLI: {error}"),
                        );
                        self.session.reduce(SessionEvent::ChildLaunchFailed);
                    }
                }
            }
            Some(Err(TryRecvError::Disconnected)) => {
                self.launch = None;
                self.push_entry(
                    EntryKind::Error,
                    "Meetlite CLI launch failed unexpectedly".into(),
                );
                self.session.reduce(SessionEvent::ChildLaunchFailed);
            }
            Some(Err(TryRecvError::Empty)) | None => {}
        }

        let mut events = Vec::new();
        let mut disconnected = false;
        if let Some(receiver) = &self.process {
            loop {
                match receiver.try_recv() {
                    Ok(event) => events.push(event),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                }
            }
        }
        for event in events {
            self.handle_process_event(event);
        }
        if disconnected && self.process.is_some() {
            self.push_entry(
                EntryKind::Error,
                "Meetlite CLI event channel disconnected unexpectedly".into(),
            );
            self.session.reduce(SessionEvent::ChildWaitFailed);
            self.finish_process();
        }

        if self
            .cancellation_deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            self.cancellation_deadline = None;
            if let Some(effect) = self.session.reduce(SessionEvent::CancellationTimedOut) {
                self.apply_effect(effect);
            }
        }

        if self.close_requested
            && self
                .close_termination_retry_at
                .is_some_and(|retry_at| Instant::now() >= retry_at)
        {
            self.close_termination_retry_at = None;
            self.request_close_termination();
        }

        if self.launch.is_some()
            || self.process.is_some()
            || self.config_launch.is_some()
            || self.config_process.is_some()
        {
            context.request_repaint_after(Duration::from_millis(50));
        }
    }

    fn handle_process_event(&mut self, event: ProcessEvent) {
        match event {
            ProcessEvent::Stdout(event) => self.handle_cli_event(event),
            ProcessEvent::ProtocolError(error) => {
                self.push_entry(EntryKind::Error, error);
                self.session.reduce(SessionEvent::ErrorReceived);
                if let Some(controller) = &self.controller {
                    let _ = controller.terminate();
                }
            }
            ProcessEvent::Stderr(line)
                if Self::is_credential_notice(&line) && self.credential_notices.contains(&line) => {
            }
            ProcessEvent::Stderr(line) => self.push_entry(EntryKind::Warning, line),
            ProcessEvent::InterruptDelivered => {
                self.push_entry(EntryKind::Status, "Stop signal delivered".into());
                if let Some(effect) = self.session.reduce(SessionEvent::SignalDelivered) {
                    self.apply_effect(effect);
                }
            }
            ProcessEvent::InterruptFailed(error) => {
                self.session.reduce(SessionEvent::SignalDeliveryFailed);
                self.push_entry(
                    EntryKind::Error,
                    format!("Could not signal Meetlite CLI: {error}"),
                );
            }
            ProcessEvent::TerminationDelivered => {
                self.session.reduce(SessionEvent::TerminationDelivered);
                if self.close_termination_pending {
                    self.close_termination_pending = false;
                    self.close_termination_delivered = true;
                }
            }
            ProcessEvent::TerminationFailed(error) => {
                self.session.reduce(SessionEvent::TerminationFailed);
                self.close_termination_pending = false;
                if self.close_requested {
                    self.close_termination_retry_at =
                        Some(Instant::now() + CLOSE_TERMINATION_RETRY);
                }
                self.push_entry(
                    EntryKind::Error,
                    format!("Could not terminate Meetlite CLI: {error}"),
                );
            }
            ProcessEvent::Exited(status) => {
                let prior_state = self.session.state();
                let had_error = self.session.error_observed();
                self.session.reduce(SessionEvent::ChildExited {
                    success: status.success(),
                });
                if self.session.state() == SessionState::Failed
                    && prior_state != SessionState::Failed
                    && !had_error
                {
                    self.push_entry(
                        EntryKind::Error,
                        format!("Meetlite CLI exited with {status}"),
                    );
                }
                self.finish_process();
            }
            ProcessEvent::WaitFailed(error) => {
                self.push_entry(
                    EntryKind::Error,
                    format!("Could not wait for Meetlite CLI: {error}"),
                );
                self.session.reduce(SessionEvent::ChildWaitFailed);
                self.finish_process();
            }
        }
    }

    fn finish_process(&mut self) {
        self.cancellation_deadline = None;
        self.close_termination_pending = false;
        self.close_termination_retry_at = None;
        self.process = None;
        self.controller = None;
    }

    fn handle_cli_event(&mut self, event: CliEvent) {
        if let CliEvent::Lifecycle {
            phase: LifecyclePhase::RecordingStarted,
            summary_enabled,
            ..
        } = &event
        {
            self.session
                .observe_recording_summary_enabled(*summary_enabled);
        }
        if let Some(session_event) = event.session_event() {
            self.session.reduce(session_event);
        }
        match event {
            CliEvent::Lifecycle { phase, .. } => self.push_entry(
                EntryKind::Status,
                match phase {
                    LifecyclePhase::RecordingStarted => "Recording started",
                    LifecyclePhase::RecordingStopped => "Recording stopped",
                    LifecyclePhase::ProcessingStarted => "Processing recording",
                    LifecyclePhase::SummarizingStarted => "Creating summary",
                }
                .into(),
            ),
            CliEvent::TranscriptionChunk {
                source,
                start_seconds,
                text,
                ..
            } => self.push_entry(
                EntryKind::Transcript,
                format!(
                    "[{start_seconds:>8.2}s] {}: {}",
                    source_speaker(source.as_deref()),
                    text.trim()
                ),
            ),
            CliEvent::TranscriptionChunkFailed {
                chunk_index,
                source,
                start_seconds,
                error,
            } => self.push_entry(
                EntryKind::Warning,
                format!(
                    "{} transcription chunk {chunk_index} at {start_seconds:.2}s failed: {error}",
                    source_speaker(source.as_deref())
                ),
            ),
            CliEvent::TranscriptionCompleted {
                transcript_path,
                transcript,
            } => {
                self.replace_transcript_entries(&transcript);
                self.push_entry(
                    EntryKind::Status,
                    format!("Transcript saved to {}", transcript_path.display()),
                );
            }
            CliEvent::SummaryDelta { text } => self.append_summary_delta(text),
            CliEvent::SummaryCompleted { summary_path, .. } => self.push_entry(
                EntryKind::Status,
                format!("Summary saved to {}", summary_path.display()),
            ),
            CliEvent::Error { message } => self.push_entry(EntryKind::Error, message),
        }
    }

    fn replace_transcript_entries(&mut self, transcript: &serde_json::Value) {
        let entries = final_transcript_entries(transcript);
        if entries.is_empty() {
            return;
        }
        self.entries
            .retain(|entry| entry.kind != EntryKind::Transcript);
        for text in entries {
            self.push_entry(EntryKind::Transcript, text);
        }
    }

    fn push_entry(&mut self, kind: EntryKind, text: String) {
        self.summary_entry = None;
        let text = if kind == EntryKind::Transcript {
            text
        } else {
            truncate_entry_text(text)
        };
        self.entries.push(Entry { kind, text });
        self.enforce_entry_limits();
    }

    fn remove_entry(&mut self, index: usize) {
        self.entries.remove(index);
        if let Some(summary) = self.summary_entry {
            self.summary_entry = if summary == index {
                None
            } else if summary > index {
                Some(summary - 1)
            } else {
                Some(summary)
            };
        }
    }

    fn enforce_entry_limits(&mut self) {
        let marker_present = self
            .entries
            .iter()
            .any(|entry| entry.kind == EntryKind::Status && entry.text == HISTORY_TRUNCATED);
        let mut count = self
            .entries
            .iter()
            .filter(|entry| entry.kind != EntryKind::Transcript)
            .count();
        let mut bytes = self
            .entries
            .iter()
            .filter(|entry| entry.kind != EntryKind::Transcript)
            .map(|entry| entry.text.len())
            .sum::<usize>();
        let reserved_count = usize::from(!marker_present);
        let reserved_bytes = if marker_present {
            0
        } else {
            HISTORY_TRUNCATED.len()
        };
        let mut removed = false;
        while count + reserved_count > MAX_NON_TRANSCRIPT_ENTRIES
            || bytes + reserved_bytes > MAX_NON_TRANSCRIPT_BYTES
        {
            let Some(index) = self.entries.iter().position(|entry| {
                entry.kind != EntryKind::Transcript && entry.text != HISTORY_TRUNCATED
            }) else {
                break;
            };
            count -= 1;
            bytes = bytes.saturating_sub(self.entries[index].text.len());
            self.remove_entry(index);
            removed = true;
        }
        if removed && !marker_present {
            self.entries.insert(
                0,
                Entry {
                    kind: EntryKind::Status,
                    text: HISTORY_TRUNCATED.into(),
                },
            );
            if let Some(summary) = &mut self.summary_entry {
                *summary += 1;
            }
        }
    }

    fn append_summary_delta(&mut self, text: String) {
        if let Some(index) = self.summary_entry {
            if !self.entries[index].text.ends_with(ENTRY_TRUNCATION_SUFFIX) {
                self.entries[index].text.push_str(&text);
                self.entries[index].text =
                    truncate_entry_text(std::mem::take(&mut self.entries[index].text));
            }
        } else {
            self.entries.push(Entry {
                kind: EntryKind::Summary,
                text: truncate_entry_text(text),
            });
            self.summary_entry = Some(self.entries.len() - 1);
        }
        self.enforce_entry_limits();
    }

    fn status(&self) -> &'static str {
        match self.session.state() {
            SessionState::SetupRequired => "Setup required",
            SessionState::Ready => "Ready to record",
            SessionState::Recording => "Recording",
            SessionState::Processing => "Processing",
            SessionState::Summarizing => "Summarizing",
            SessionState::Terminating => "Stopping",
            SessionState::Complete => "Complete",
            SessionState::Stopped => "Stopped",
            SessionState::Failed => "Failed",
        }
    }

    fn show_config_form(&mut self, ui: &mut egui::Ui) {
        let onboarding = self.config_state.onboarding();
        let saving = matches!(self.config_state, ConfigState::Saving { .. });
        let busy = saving || self.config_launch.is_some() || self.config_process.is_some();
        ui.horizontal(|ui| {
            ui.heading(if onboarding {
                "Set up Meetlite"
            } else {
                "Settings"
            });
            if !onboarding {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add_enabled(!saving, egui::Button::new("Close"))
                        .clicked()
                    {
                        self.config_state.reduce(ConfigAction::Cancel);
                        self.config_error = None;
                    }
                });
            }
        });
        if onboarding {
            ui.label("Configure transcription to start recording.");
            if let Some(reason) = &self.config_reason {
                ui.label(RichText::new(reason).color(Color32::from_rgb(220, 70, 70)));
            }
        }
        ui.add_space(8.0);
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.add_enabled_ui(!saving, |ui| {
                    ui.strong("Speech-to-text");
                    ui.label("Base URL");
                    ui.text_edit_singleline(&mut self.config_form.stt_base_url);
                    ui.label("Model");
                    ui.text_edit_singleline(&mut self.config_form.stt_model);
                    ui.label("Transcription prompt (optional)");
                    ui.add(
                        egui::TextEdit::multiline(&mut self.config_form.stt_prompt).desired_rows(3),
                    );
                    ui.label("API key");
                    ui.horizontal(|ui| {
                        secret_input(
                            ui,
                            "stt-api-key",
                            &mut self.config_form.stt_api_key,
                            &mut self.config_form.show_stt_api_key,
                        );
                    });
                    if self.config_form.stt_managed_credential {
                        ui.small("Leave ******** unchanged to preserve; clear it to remove");
                    } else if self.config_form.stt_auth_source != "none" {
                        ui.small(format!(
                            "Configured via {}; leave empty to preserve",
                            self.config_form.stt_auth_source.replace('_', " ")
                        ));
                    }
                    self.config_form.reconcile_key_visibility();
                    ui.add_space(10.0);
                    ui.checkbox(&mut self.config_form.summary_enabled, "Enable summary");
                    ui.add_enabled_ui(self.config_form.summary_enabled, |ui| {
                        ui.strong("Summary model");
                        ui.label("Base URL");
                        ui.text_edit_singleline(&mut self.config_form.llm_base_url);
                        ui.label("Model");
                        ui.text_edit_singleline(&mut self.config_form.llm_model);
                        ui.label("API key");
                        ui.horizontal(|ui| {
                            secret_input(
                                ui,
                                "llm-api-key",
                                &mut self.config_form.llm_api_key,
                                &mut self.config_form.show_llm_api_key,
                            );
                        });
                        if self.config_form.llm_managed_credential {
                            ui.small("Leave ******** unchanged to preserve; clear it to remove");
                        } else if self.config_form.llm_auth_source != "none" {
                            ui.small(format!(
                                "Configured via {}; leave empty to preserve",
                                self.config_form.llm_auth_source.replace('_', " ")
                            ));
                        }
                        self.config_form.reconcile_key_visibility();
                        ui.label("Summary instructions (optional)");
                        ui.add(
                            egui::TextEdit::multiline(&mut self.config_form.instructions)
                                .desired_rows(3),
                        );
                    });
                });
                if let Some(error) = &self.config_error {
                    ui.add_space(8.0);
                    ui.label(RichText::new(error).color(Color32::from_rgb(220, 70, 70)));
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.add_enabled(!saving, egui::Button::new("Save")).clicked() {
                        if let Some(effect) = self.config_state.reduce(ConfigAction::Save) {
                            self.apply_config_effect(effect);
                        }
                    }
                    if matches!(self.config_state, ConfigState::ApplyError { .. }) {
                        if ui.add_enabled(!busy, egui::Button::new("Retry")).clicked() {
                            if let Some(effect) = self.config_state.reduce(ConfigAction::Retry) {
                                self.apply_config_effect(effect);
                            }
                        }
                        if ui
                            .add_enabled(!busy, egui::Button::new("Use existing configuration"))
                            .clicked()
                        {
                            if let Some(effect) =
                                self.config_state.reduce(ConfigAction::UseExisting)
                            {
                                self.apply_config_effect(effect);
                            }
                        }
                    }
                    if saving {
                        ui.spinner();
                    }
                });
            });
    }

    fn show_config_error(&mut self, ui: &mut egui::Ui) {
        ui.vertical_centered(|ui| {
            ui.heading("Configuration unavailable");
            ui.add_space(16.0);
            if let Some(error) = &self.config_error {
                ui.label(RichText::new(error).color(Color32::from_rgb(220, 70, 70)));
            }
            ui.add_space(12.0);
            if ui
                .add_enabled(
                    self.config_launch.is_none() && self.config_process.is_none(),
                    egui::Button::new("Retry"),
                )
                .clicked()
            {
                if let Some(effect) = self.config_state.reduce(ConfigAction::Retry) {
                    self.apply_config_effect(effect);
                }
            }
        });
    }

    fn show_main(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Meetlite");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled(
                        self.configuration_available(),
                        egui::Button::new("Settings"),
                    )
                    .on_hover_text("Open settings")
                    .clicked()
                {
                    self.open_settings();
                }
                let transcript = transcript_text(&self.entries);
                if ui
                    .add_enabled(transcript.is_some(), egui::Button::new("Copy transcript"))
                    .on_hover_text("Copy current transcript")
                    .clicked()
                {
                    ui.ctx().copy_text(transcript.unwrap());
                }
            });
        });
        ui.vertical_centered(|ui| {
            ui.label(self.status());
            ui.add_space(12.0);
            self.show_primary_control(ui);
            ui.add_space(14.0);
        });
        ui.separator();
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .stick_to_bottom(true)
            .show(ui, |ui| {
                for entry in &self.entries {
                    let color = match entry.kind {
                        EntryKind::Transcript | EntryKind::Summary => ui.visuals().text_color(),
                        EntryKind::Status => Color32::from_rgb(80, 150, 220),
                        EntryKind::Warning => Color32::from_rgb(220, 150, 40),
                        EntryKind::Error => Color32::from_rgb(220, 70, 70),
                    };
                    ui.label(RichText::new(&entry.text).color(color));
                }
            });
    }

    fn show_primary_control(&mut self, ui: &mut egui::Ui) {
        let mut spec = self.session.control_spec();
        if matches!(
            self.session.state(),
            SessionState::Complete | SessionState::Stopped | SessionState::Failed
        ) && (self.launch.is_some() || self.process.is_some())
        {
            spec.enabled = false;
        }
        let response = match spec.visual {
            ControlVisual::Button(label) => ui.add_enabled(
                spec.enabled,
                egui::Button::new(RichText::new(label).size(24.0)).min_size(CONTROL_SIZE),
            ),
            ControlVisual::Record => {
                ui.add_enabled_ui(spec.enabled, |ui| {
                    let (rect, response) = ui.allocate_exact_size(CONTROL_SIZE, Sense::click());
                    let visuals = ui.style().interact(&response);
                    ui.painter().rect(
                        rect,
                        visuals.corner_radius,
                        visuals.bg_fill,
                        visuals.bg_stroke,
                        egui::StrokeKind::Inside,
                    );
                    ui.painter()
                        .circle_filled(rect.center(), 20.0, Color32::from_rgb(220, 45, 45));
                    response
                })
                .inner
            }
            ControlVisual::Spinner => {
                ui.add_enabled_ui(spec.enabled, |ui| {
                    let (rect, response) = ui.allocate_exact_size(CONTROL_SIZE, Sense::click());
                    let visuals = ui.style().interact(&response);
                    ui.painter().rect(
                        rect,
                        visuals.corner_radius,
                        visuals.bg_fill,
                        visuals.bg_stroke,
                        egui::StrokeKind::Inside,
                    );
                    if spinner_shows_stop(self.session.state(), spec.enabled, response.hovered()) {
                        ui.painter().text(
                            rect.center(),
                            egui::Align2::CENTER_CENTER,
                            "Stop",
                            egui::FontId::proportional(24.0),
                            visuals.fg_stroke.color,
                        );
                    } else {
                        egui::Spinner::new().size(42.0).paint_at(ui, rect);
                    }
                    response
                })
                .inner
            }
        };
        let clicked = if let Some(tooltip) = spec.tooltip {
            response.on_hover_text(tooltip).clicked()
        } else {
            response.clicked()
        };
        if clicked {
            self.primary_control();
        }
    }
}

impl eframe::App for MeetliteApp {
    fn logic(&mut self, context: &egui::Context, _frame: &mut eframe::Frame) {
        if !self.config_started {
            self.config_started = true;
            self.start_config_probe();
        }
        if context.input(|input| input.viewport().close_requested()) {
            if self.launch.is_some()
                || self.process.is_some()
                || self.config_launch.is_some()
                || self.config_process.is_some()
            {
                context.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.request_close();
            }
        }
        self.drain_messages(context);
        if self.ready_to_close() {
            context.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ui, |ui| {
            ui.add_space(8.0);
            if self.config_state.form_visible() {
                self.show_config_form(ui);
            } else if self.config_state.error_visible() {
                self.show_config_error(ui);
            } else if matches!(self.config_state, ConfigState::Checking { .. }) {
                ui.vertical_centered(|ui| {
                    ui.heading("Meetlite");
                    ui.add_space(24.0);
                    ui.spinner();
                    ui.label("Checking configuration…");
                    for notice in &self.credential_notices {
                        ui.add_space(8.0);
                        ui.label(RichText::new(notice).color(Color32::from_rgb(80, 150, 220)));
                    }
                });
            } else {
                self.show_main(ui);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;

    fn exit_status(command: &str) -> std::process::ExitStatus {
        Command::new("/bin/sh")
            .args(["-c", command])
            .status()
            .unwrap()
    }

    fn advance_to_summarizing(app: &mut MeetliteApp) {
        app.session.reduce(SessionEvent::PrimaryControlPressed);
        app.session.reduce(SessionEvent::RecordingStarted);
        app.session.reduce(SessionEvent::RecordingStopped);
        app.session.reduce(SessionEvent::SummarizingStarted);
    }

    #[test]
    fn only_actionable_spinners_show_stop_on_hover() {
        assert!(spinner_shows_stop(SessionState::Processing, true, true));
        assert!(spinner_shows_stop(SessionState::Summarizing, true, true));
        assert!(!spinner_shows_stop(SessionState::Terminating, false, true));
        assert!(!spinner_shows_stop(SessionState::Processing, true, false));
    }

    #[test]
    fn font_selection_prefers_one_readable_broad_candidate() {
        let candidates = [
            FontCandidate {
                path: "/broad",
                index: 2,
                coverage: CJK_ALL,
            },
            FontCandidate {
                path: "/chinese",
                index: 0,
                coverage: CJK_CHINESE,
            },
            FontCandidate {
                path: "/japanese",
                index: 0,
                coverage: CJK_JAPANESE,
            },
            FontCandidate {
                path: "/korean",
                index: 0,
                coverage: CJK_KOREAN,
            },
        ];

        assert_eq!(
            select_font_candidates(&candidates, |path| path != Path::new("/missing")),
            [candidates[0]]
        );
    }

    #[test]
    fn font_selection_skips_unreadable_candidates_and_fills_missing_coverage() {
        let candidates = [
            FontCandidate {
                path: "/missing",
                index: 0,
                coverage: CJK_ALL,
            },
            FontCandidate {
                path: "/chinese",
                index: 1,
                coverage: CJK_CHINESE,
            },
            FontCandidate {
                path: "/japanese-korean",
                index: 3,
                coverage: CJK_JAPANESE | CJK_KOREAN,
            },
        ];

        assert_eq!(
            select_font_candidates(&candidates, |path| path != Path::new("/missing")),
            [candidates[1], candidates[2]]
        );
        assert!(select_font_candidates(&candidates, |_| false).is_empty());
    }

    #[test]
    fn font_fallbacks_append_to_both_default_families_and_keep_face_indices() {
        let mut definitions = FontDefinitions::default();
        let proportional = definitions.families[&FontFamily::Proportional].clone();
        let monospace = definitions.families[&FontFamily::Monospace].clone();
        let mut first = FontData::from_owned(vec![1]);
        first.index = 4;
        let mut second = FontData::from_owned(vec![2]);
        second.index = 7;

        append_font_fallbacks(
            &mut definitions,
            [("cjk-one".into(), first), ("cjk-two".into(), second)],
        );

        assert_eq!(
            &definitions.families[&FontFamily::Proportional][..proportional.len()],
            proportional
        );
        assert_eq!(
            &definitions.families[&FontFamily::Monospace][..monospace.len()],
            monospace
        );
        assert_eq!(
            &definitions.families[&FontFamily::Proportional][proportional.len()..],
            ["cjk-one", "cjk-two"]
        );
        assert_eq!(
            &definitions.families[&FontFamily::Monospace][monospace.len()..],
            ["cjk-one", "cjk-two"]
        );
        assert_eq!(definitions.font_data["cjk-one"].index, 4);
        assert_eq!(definitions.font_data["cjk-two"].index, 7);
    }

    #[test]
    fn transcript_copy_text_includes_only_current_transcript_entries() {
        let entries = [
            Entry {
                kind: EntryKind::Status,
                text: "Recording started".into(),
            },
            Entry {
                kind: EntryKind::Transcript,
                text: "[    0.00s] Meeting: Hello".into(),
            },
            Entry {
                kind: EntryKind::Summary,
                text: "Summary".into(),
            },
            Entry {
                kind: EntryKind::Transcript,
                text: "[    3.25s] Remote: Hi".into(),
            },
        ];

        assert_eq!(
            transcript_text(&entries).as_deref(),
            Some("[    0.00s] Meeting: Hello\n[    3.25s] Remote: Hi")
        );
        assert_eq!(transcript_text(&entries[..1]), None);
    }

    #[test]
    fn entry_cap_removes_diagnostics_but_preserves_current_final_transcript() {
        let mut app = MeetliteApp::default();
        app.handle_cli_event(CliEvent::TranscriptionCompleted {
            transcript_path: "transcript.json".into(),
            transcript: serde_json::json!({
                "segments": [{
                    "start_seconds": 1.0,
                    "text": "authoritative final transcript",
                    "speaker": "You"
                }]
            }),
        });
        let expected = transcript_text(&app.entries).unwrap();
        for index in 0..(MAX_NON_TRANSCRIPT_ENTRIES * 2) {
            app.push_entry(EntryKind::Warning, format!("diagnostic {index}"));
        }

        assert_eq!(
            transcript_text(&app.entries).as_deref(),
            Some(expected.as_str())
        );
        assert!(app.entries.len() <= MAX_NON_TRANSCRIPT_ENTRIES + 1);
        assert!(app
            .entries
            .iter()
            .any(|entry| entry.text == HISTORY_TRUNCATED));
        assert!(!app.entries.iter().any(|entry| entry.text == "diagnostic 0"));
        assert!(app.entries.iter().any(
            |entry| entry.text == format!("diagnostic {}", MAX_NON_TRANSCRIPT_ENTRIES * 2 - 1)
        ));
    }

    #[test]
    fn oversized_diagnostic_entry_is_visibly_truncated() {
        let mut app = MeetliteApp::default();
        let secret_tail = "do-not-display";
        app.push_entry(
            EntryKind::Warning,
            format!("{}{secret_tail}", "x".repeat(MAX_ENTRY_BYTES + 1)),
        );

        let text = &app.entries[0].text;
        assert!(text.len() <= MAX_ENTRY_BYTES);
        assert!(text.ends_with(ENTRY_TRUNCATION_SUFFIX));
        assert!(!text.contains(secret_tail));
    }

    #[test]
    fn transcript_chunks_and_summary_deltas_never_share_accumulation() {
        let mut app = MeetliteApp::default();
        app.handle_cli_event(CliEvent::TranscriptionChunk {
            chunk_index: 0,
            source: Some("microphone".into()),
            start_seconds: 0.0,
            text: "transcript".into(),
        });
        app.handle_cli_event(CliEvent::SummaryDelta {
            text: "summary ".into(),
        });
        app.handle_cli_event(CliEvent::SummaryDelta {
            text: "continued".into(),
        });

        assert_eq!(
            app.entries,
            [
                Entry {
                    kind: EntryKind::Transcript,
                    text: "[    0.00s] You: transcript".into(),
                },
                Entry {
                    kind: EntryKind::Summary,
                    text: "summary continued".into(),
                },
            ]
        );
    }

    #[test]
    fn final_transcript_replaces_live_chunks_with_timestamped_speakers() {
        let mut app = MeetliteApp::default();
        app.handle_cli_event(CliEvent::TranscriptionChunk {
            chunk_index: 0,
            source: Some("microphone".into()),
            start_seconds: 0.0,
            text: "source transcript".into(),
        });
        app.handle_cli_event(CliEvent::TranscriptionCompleted {
            transcript_path: "transcript.json".into(),
            transcript: serde_json::json!({
                "text": "You: Hello.\nRemote: Hi.",
                "segments": [
                    {
                        "start_seconds": 1.25,
                        "end_seconds": 2.0,
                        "text": "Hello.",
                        "speaker": "You"
                    },
                    {
                        "start_seconds": 2.5,
                        "end_seconds": 3.0,
                        "text": "Hi.",
                        "speaker": "Remote"
                    }
                ]
            }),
        });

        assert!(!app
            .entries
            .iter()
            .any(|entry| entry.text.contains("source transcript")));
        assert!(app
            .entries
            .iter()
            .any(|entry| entry.text == "[    1.25s] You: Hello."));
        assert!(app
            .entries
            .iter()
            .any(|entry| entry.text == "[    2.50s] Remote: Hi."));
    }

    #[test]
    fn final_transcript_without_speakers_uses_meeting_label() {
        assert_eq!(
            final_transcript_entries(&serde_json::json!({
                "text": "General discussion",
                "segments": []
            })),
            ["[    0.00s] Meeting: General discussion"]
        );
    }

    #[test]
    fn intervening_events_preserve_chronological_summary_groups() {
        let mut app = MeetliteApp::default();
        app.handle_cli_event(CliEvent::SummaryDelta { text: "one".into() });
        app.handle_cli_event(CliEvent::TranscriptionChunkFailed {
            chunk_index: 1,
            source: Some("system".into()),
            start_seconds: 1.0,
            error: "bad".into(),
        });
        app.handle_cli_event(CliEvent::SummaryDelta { text: "two".into() });

        assert_eq!(app.entries.len(), 3);
        assert_eq!(app.entries[0].text, "one");
        assert_eq!(app.entries[1].kind, EntryKind::Warning);
        assert_eq!(app.entries[2].text, "two");
    }

    #[test]
    fn delivered_summary_interrupt_starts_grace_period() {
        let mut app = MeetliteApp::default();
        advance_to_summarizing(&mut app);
        app.session.reduce(SessionEvent::PrimaryControlPressed);
        app.handle_process_event(ProcessEvent::InterruptDelivered);
        assert!(app.cancellation_deadline.is_some());
        assert_eq!(app.session.state(), SessionState::Summarizing);
    }

    #[test]
    fn timeout_waits_for_terminate_ack_and_expected_exit_stays_stopped() {
        let mut app = MeetliteApp::default();
        advance_to_summarizing(&mut app);
        app.session.reduce(SessionEvent::PrimaryControlPressed);
        app.session.reduce(SessionEvent::SignalDelivered);
        app.session.reduce(SessionEvent::CancellationTimedOut);
        assert_eq!(app.session.state(), SessionState::Terminating);

        app.handle_process_event(ProcessEvent::TerminationDelivered);
        assert_eq!(app.session.state(), SessionState::Stopped);
        app.handle_process_event(ProcessEvent::Exited(exit_status("exit 9")));

        assert_eq!(app.session.state(), SessionState::Stopped);
        assert!(!app
            .entries
            .iter()
            .any(|entry| entry.text.starts_with("Meetlite CLI exited with")));
    }

    #[test]
    fn terminate_failure_is_visible_and_leaves_retry_available() {
        let mut app = MeetliteApp::default();
        advance_to_summarizing(&mut app);
        app.session.reduce(SessionEvent::PrimaryControlPressed);
        app.session.reduce(SessionEvent::SignalDelivered);
        app.session.reduce(SessionEvent::CancellationTimedOut);
        app.handle_process_event(ProcessEvent::TerminationFailed("refused".into()));

        assert_eq!(app.session.state(), SessionState::Terminating);
        assert!(app.session.control_spec().enabled);
        assert_eq!(app.entries.last().unwrap().kind, EntryKind::Error);
        assert_eq!(
            app.session.reduce(SessionEvent::PrimaryControlPressed),
            Some(SessionEffect::TerminateChild)
        );
    }

    #[test]
    fn close_waits_for_recording_exit_and_retries_termination_failure() {
        let mut delivered = MeetliteApp::default();
        let (_sender, receiver) = mpsc::channel();
        delivered.process = Some(receiver);
        delivered.close_requested = true;
        delivered.close_termination_pending = true;
        assert!(!delivered.ready_to_close());
        delivered.handle_process_event(ProcessEvent::TerminationDelivered);
        assert!(!delivered.close_termination_pending);
        assert!(delivered.close_termination_delivered);
        delivered.request_close_termination();
        assert!(!delivered.close_termination_pending);
        assert!(!delivered.ready_to_close());
        delivered.handle_process_event(ProcessEvent::Exited(exit_status("exit 9")));
        assert!(delivered.ready_to_close());

        let mut failed = MeetliteApp::default();
        let (_sender, receiver) = mpsc::channel();
        failed.process = Some(receiver);
        failed.close_requested = true;
        failed.close_termination_pending = true;
        failed.handle_process_event(ProcessEvent::TerminationFailed("refused".into()));
        assert!(!failed.ready_to_close());
        assert!(failed.close_termination_retry_at.is_some());
        assert_eq!(failed.entries.last().unwrap().kind, EntryKind::Error);
        failed.handle_process_event(ProcessEvent::Exited(exit_status("exit 9")));
        assert!(failed.ready_to_close());
    }

    #[test]
    fn unstructured_crash_adds_one_generic_error() {
        let mut app = MeetliteApp::default();
        app.session.reduce(SessionEvent::PrimaryControlPressed);
        app.session.reduce(SessionEvent::RecordingStarted);
        app.handle_process_event(ProcessEvent::Exited(exit_status("exit 7")));

        assert_eq!(app.session.state(), SessionState::Failed);
        assert_eq!(
            app.entries
                .iter()
                .filter(|entry| entry.text.starts_with("Meetlite CLI exited with"))
                .count(),
            1
        );
    }

    #[test]
    fn status_drives_onboarding_form_and_session_summary_behavior() {
        let mut app = MeetliteApp::default();
        app.handle_config_process_event(ConfigProcessEvent::Stdout(ConfigEvent::ConfigStatus {
            path: "/tmp/config.json".into(),
            usable: false,
            reason: Some("missing_config".into()),
            summary_enabled: false,
            stt: config::ProviderStatus {
                base_url: "https://stt.example/v1".into(),
                model: "speech".into(),
                prompt: Some("Product names".into()),
                credential_configured: false,
                managed_credential_configured: None,
                auth_source: "none".into(),
                auth_provenance: "default".into(),
            },
            llm: config::LlmStatus {
                base_url: "https://llm.example/v1".into(),
                model: "chat".into(),
                credential_configured: false,
                managed_credential_configured: None,
                auth_source: "none".into(),
                auth_provenance: "default".into(),
                instructions: None,
            },
        }));
        app.handle_config_process_event(ConfigProcessEvent::Exited(exit_status("exit 0")));

        assert_eq!(app.config_state, ConfigState::Editing { onboarding: true });
        assert_eq!(app.session.state(), SessionState::SetupRequired);
        assert!(!app.config_form.summary_enabled);
    }

    #[test]
    fn no_summary_app_completes_after_transcript_and_successful_exit() {
        let mut app = MeetliteApp::default();
        app.session.set_summary_enabled(false);
        app.session.reduce(SessionEvent::PrimaryControlPressed);
        app.session.reduce(SessionEvent::RecordingStarted);
        app.session.reduce(SessionEvent::ProcessingStarted);
        app.handle_cli_event(CliEvent::TranscriptionCompleted {
            transcript_path: "transcript.json".into(),
            transcript: serde_json::json!({}),
        });
        app.handle_process_event(ProcessEvent::Exited(exit_status("exit 0")));

        assert_eq!(app.session.state(), SessionState::Complete);
    }

    #[test]
    fn failed_probe_never_opens_the_default_form() {
        let mut app = MeetliteApp::default();
        app.handle_config_process_event(ConfigProcessEvent::ProtocolError("bad status".into()));
        app.handle_config_process_event(ConfigProcessEvent::Exited(exit_status("exit 0")));

        assert_eq!(
            app.config_state,
            ConfigState::ProbeError { onboarding: true }
        );
        assert!(!app.config_state.form_visible());
        assert_eq!(app.config_error.as_deref(), Some("bad status"));
    }

    #[test]
    fn unusable_status_exposes_its_redacted_reason() {
        let mut app = MeetliteApp::default();
        app.handle_config_process_event(ConfigProcessEvent::Stdout(ConfigEvent::ConfigStatus {
            path: "/tmp/config.json".into(),
            usable: false,
            reason: Some("credential_store_unavailable".into()),
            summary_enabled: true,
            stt: config::ProviderStatus {
                base_url: "https://stt.example/v1".into(),
                model: "speech".into(),
                prompt: Some("Product names".into()),
                credential_configured: false,
                managed_credential_configured: None,
                auth_source: "none".into(),
                auth_provenance: "default".into(),
            },
            llm: config::LlmStatus {
                base_url: "https://llm.example/v1".into(),
                model: "chat".into(),
                credential_configured: false,
                managed_credential_configured: None,
                auth_source: "none".into(),
                auth_provenance: "default".into(),
                instructions: None,
            },
        }));
        app.handle_config_process_event(ConfigProcessEvent::Exited(exit_status("exit 0")));

        assert_eq!(
            app.config_reason.as_deref(),
            Some("credential_store_unavailable")
        );
        assert_eq!(app.config_state, ConfigState::Editing { onboarding: true });
    }

    #[test]
    fn failed_apply_keeps_keys_and_retry_reapplies_without_leaking_errors() {
        let mut app = MeetliteApp::default();
        app.config_state = ConfigState::Saving { onboarding: false };
        app.config_form.stt_api_key = "stt-secret".into();
        app.config_form.llm_api_key = "llm-secret".into();
        app.handle_config_process_event(ConfigProcessEvent::Stdout(ConfigEvent::ConfigSaved {
            path: "/tmp/config.json".into(),
            stored_credentials: Vec::new(),
        }));
        app.handle_config_process_event(ConfigProcessEvent::Stdout(ConfigEvent::Error {
            message: "rejected stt-secret and llm-secret".into(),
        }));
        app.handle_config_process_event(ConfigProcessEvent::Exited(exit_status("exit 0")));

        assert_eq!(
            app.config_state,
            ConfigState::ApplyError { onboarding: false }
        );
        assert_eq!(app.config_form.stt_api_key, "stt-secret");
        assert_eq!(app.config_form.llm_api_key, "llm-secret");
        assert_eq!(
            app.config_error.as_deref(),
            Some("rejected [redacted] and [redacted]")
        );
        assert_eq!(
            app.config_state.reduce(ConfigAction::Retry),
            Some(ConfigEffect::Apply)
        );
    }

    #[test]
    fn save_requires_saved_event_clean_protocol_and_successful_exit() {
        for events in [0, 2] {
            let mut app = MeetliteApp::default();
            app.config_state = ConfigState::Saving { onboarding: true };
            app.config_form.stt_api_key = "masked-secret".into();
            if events != 0 {
                app.handle_config_process_event(ConfigProcessEvent::Stdout(
                    ConfigEvent::ConfigSaved {
                        path: "/tmp/config.json".into(),
                        stored_credentials: Vec::new(),
                    },
                ));
            }
            if events == 2 {
                app.handle_config_process_event(ConfigProcessEvent::InputFailed(
                    "input failed".into(),
                ));
            }
            app.handle_config_process_event(ConfigProcessEvent::Exited(exit_status("exit 0")));
            assert_eq!(
                app.config_state,
                ConfigState::ApplyError { onboarding: true }
            );
            assert_eq!(app.config_form.stt_api_key, "masked-secret");
        }

        let mut failed_exit = MeetliteApp::default();
        failed_exit.config_state = ConfigState::Saving { onboarding: false };
        failed_exit.config_form.stt_api_key = "masked-secret".into();
        failed_exit.handle_config_process_event(ConfigProcessEvent::Stdout(
            ConfigEvent::ConfigSaved {
                path: "/tmp/config.json".into(),
                stored_credentials: Vec::new(),
            },
        ));
        failed_exit.handle_config_process_event(ConfigProcessEvent::Exited(exit_status("exit 1")));
        assert_eq!(
            failed_exit.config_state,
            ConfigState::ApplyError { onboarding: false }
        );
        assert_eq!(failed_exit.config_form.stt_api_key, "masked-secret");
    }

    #[test]
    fn accepted_save_clears_masked_keys_and_reprobes() {
        let mut app = MeetliteApp::default();
        app.config_state = ConfigState::Saving { onboarding: false };
        app.config_form.stt_api_key = "stt-secret".into();
        app.config_form.llm_api_key = "llm-secret".into();

        assert_eq!(app.accept_config_save(), Some(ConfigEffect::Probe));
        assert!(app.config_form.stt_api_key.is_empty());
        assert!(app.config_form.llm_api_key.is_empty());
        assert_eq!(
            app.config_state,
            ConfigState::Checking { onboarding: false }
        );
    }

    #[test]
    fn lifecycle_summary_setting_overrides_cache_and_absence_keeps_fallback() {
        let mut disabled = MeetliteApp::default();
        disabled.session.set_summary_enabled(true);
        disabled.session.reduce(SessionEvent::PrimaryControlPressed);
        disabled.handle_cli_event(
            super::super::events::parse_line(
                r#"{"type":"lifecycle","phase":"recording_started","output_dir":"recording","summary_enabled":false}"#,
            )
            .unwrap(),
        );
        disabled.session.reduce(SessionEvent::ProcessingStarted);
        disabled
            .session
            .reduce(SessionEvent::TranscriptionCompleted);
        disabled
            .session
            .reduce(SessionEvent::ChildExited { success: true });
        assert_eq!(disabled.session.state(), SessionState::Complete);

        let mut enabled = MeetliteApp::default();
        enabled.session.set_summary_enabled(false);
        enabled.session.reduce(SessionEvent::PrimaryControlPressed);
        enabled.handle_cli_event(
            super::super::events::parse_line(
                r#"{"type":"lifecycle","phase":"recording_started","output_dir":"recording","summary_enabled":true}"#,
            )
            .unwrap(),
        );
        enabled.session.reduce(SessionEvent::ProcessingStarted);
        enabled.session.reduce(SessionEvent::TranscriptionCompleted);
        enabled
            .session
            .reduce(SessionEvent::ChildExited { success: true });
        assert_eq!(enabled.session.state(), SessionState::Failed);

        let mut fallback = MeetliteApp::default();
        fallback.session.set_summary_enabled(false);
        fallback.session.reduce(SessionEvent::PrimaryControlPressed);
        fallback.handle_cli_event(
            super::super::events::parse_line(
                r#"{"type":"lifecycle","phase":"recording_started","output_dir":"recording"}"#,
            )
            .unwrap(),
        );
        fallback.session.reduce(SessionEvent::ProcessingStarted);
        fallback
            .session
            .reduce(SessionEvent::TranscriptionCompleted);
        fallback
            .session
            .reduce(SessionEvent::ChildExited { success: true });
        assert_eq!(fallback.session.state(), SessionState::Complete);
    }

    #[test]
    fn startup_keychain_notices_are_visible_and_recording_duplicates_are_suppressed() {
        let notice = "Keychain access needed for Meetlite/Meetlite API Credentials.";
        let instruction =
            "macOS may prompt for Keychain access; choose Allow or Always Allow to continue.";
        let mut app = MeetliteApp::default();

        app.handle_config_process_event(ConfigProcessEvent::Stderr(notice.into()));
        app.handle_config_process_event(ConfigProcessEvent::Stderr(instruction.into()));
        assert_eq!(
            app.entries
                .iter()
                .map(|entry| entry.text.as_str())
                .collect::<Vec<_>>(),
            [notice, instruction]
        );
        assert!(app.config_error.is_none());

        app.handle_process_event(ProcessEvent::Stderr(notice.into()));
        app.handle_process_event(ProcessEvent::Stderr(instruction.into()));
        assert_eq!(app.entries.len(), 2);
    }

    #[test]
    fn close_waits_for_active_configuration_child_with_bounded_fallback() {
        let (sender, receiver) = mpsc::channel();
        let mut app = MeetliteApp::default();
        app.config_process = Some(receiver);
        app.close_requested = true;
        app.config_close_deadline = Some(Instant::now() + CONFIG_CLOSE_GRACE);
        assert!(!app.ready_to_close());

        sender
            .send(ConfigProcessEvent::Exited(exit_status("exit 1")))
            .unwrap();
        app.drain_config_messages();
        assert!(app.ready_to_close());

        let (_sender, receiver) = mpsc::channel();
        let mut bounded = MeetliteApp::default();
        bounded.config_process = Some(receiver);
        bounded.close_requested = true;
        bounded.config_close_deadline = Some(Instant::now() - Duration::from_millis(1));
        assert!(bounded.ready_to_close());
    }

    #[test]
    fn disconnected_launch_channels_become_visible_failures() {
        let (config_sender, config_receiver) = mpsc::channel();
        drop(config_sender);
        let mut config_app = MeetliteApp::default();
        config_app.config_launch = Some(config_receiver);
        config_app.drain_config_messages();
        assert_eq!(
            config_app.config_state,
            ConfigState::ProbeError { onboarding: true }
        );
        assert!(config_app.config_error.is_some());

        let (launch_sender, launch_receiver) = mpsc::channel();
        drop(launch_sender);
        let mut launch_app = MeetliteApp::default();
        launch_app
            .session
            .reduce(SessionEvent::PrimaryControlPressed);
        launch_app.launch = Some(launch_receiver);
        launch_app.drain_messages(&egui::Context::default());
        assert_eq!(launch_app.session.state(), SessionState::Failed);
        assert_eq!(launch_app.entries.last().unwrap().kind, EntryKind::Error);
    }

    #[test]
    fn structured_error_does_not_gain_a_generic_crash_error() {
        let mut app = MeetliteApp::default();
        app.session.reduce(SessionEvent::PrimaryControlPressed);
        app.session.reduce(SessionEvent::RecordingStarted);
        app.handle_cli_event(CliEvent::Error {
            message: "specific failure".into(),
        });
        app.handle_process_event(ProcessEvent::Exited(exit_status("exit 1")));

        assert_eq!(app.session.state(), SessionState::Failed);
        assert_eq!(app.entries.len(), 1);
        assert_eq!(app.entries[0].text, "specific failure");
    }
}
