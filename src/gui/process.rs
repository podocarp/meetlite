use std::{
    env, fs,
    io::{self, BufRead, BufReader},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender, TryRecvError, TrySendError},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

use super::events::{parse_line, CliEvent};

type SignalSender = Arc<dyn Fn(u32, libc::c_int) -> io::Result<()> + Send + Sync>;
type ChildKiller = Arc<dyn Fn(&mut Child) -> io::Result<()> + Send + Sync>;

const EXIT_DRAIN_GRACE: Duration = Duration::from_millis(250);
const MAX_PROTOCOL_LINE_BYTES: usize = 8 * 1024 * 1024;
const MAX_DIAGNOSTIC_LINE_BYTES: usize = 64 * 1024;
const PROCESS_CHANNEL_CAPACITY: usize = 256;
const PROTOCOL_TRUNCATED: &str =
    "Meetlite CLI protocol output was truncated because a line exceeded 8 MiB";
const DIAGNOSTIC_TRUNCATED: &str =
    "Meetlite CLI diagnostic output was truncated because a line exceeded 64 KiB";
const OUTPUT_DROPPED: &str = "Meetlite CLI output was truncated because the display queue was full";

#[derive(Debug)]
pub(crate) enum ProcessEvent {
    Stdout(CliEvent),
    ProtocolError(String),
    Stderr(String),
    InterruptDelivered,
    InterruptFailed(String),
    TerminationDelivered,
    TerminationFailed(String),
    Exited(ExitStatus),
    WaitFailed(String),
}

pub(crate) trait ProcessControl {
    fn interrupt(&self) -> io::Result<()>;
    fn terminate(&self) -> io::Result<()>;
}

#[derive(Debug)]
pub(crate) struct ChildController {
    commands: Sender<ProcessCommand>,
}

impl ProcessControl for ChildController {
    fn interrupt(&self) -> io::Result<()> {
        self.commands
            .send(ProcessCommand::Interrupt)
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "Meetlite CLI has exited"))
    }

    fn terminate(&self) -> io::Result<()> {
        self.commands
            .send(ProcessCommand::Terminate)
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "Meetlite CLI has exited"))
    }
}

pub(crate) fn resolve_cli() -> io::Result<PathBuf> {
    let current_exe = env::current_exe()?;
    let path = cli_path_for(&current_exe).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "Could not resolve the Meetlite CLI beside {}",
                current_exe.display()
            ),
        )
    })?;
    let metadata = fs::metadata(&path).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!(
                "Could not find the Meetlite CLI at {}: {error}",
                path.display()
            ),
        )
    })?;
    if !metadata.is_file() {
        return Err(io::Error::other(format!(
            "Meetlite CLI path is not a file: {}",
            path.display()
        )));
    }
    Ok(path)
}

pub(crate) fn cli_path_for(gui_executable: &Path) -> Option<PathBuf> {
    let executable_dir = gui_executable.parent()?;
    if executable_dir
        .file_name()
        .is_some_and(|name| name == "MacOS")
        && executable_dir
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|name| name == "Contents")
    {
        return Some(executable_dir.parent()?.join("Resources").join("meetlite"));
    }
    Some(executable_dir.join("meetlite"))
}

pub(crate) fn spawn_cli() -> io::Result<(ChildController, Receiver<ProcessEvent>)> {
    spawn_command(cli_command(resolve_cli()?))
}

fn cli_command(executable: PathBuf) -> Command {
    let mut command = Command::new(executable);
    command.args(["--json", "start"]);
    if let Some(directory) = launch_directory() {
        command.current_dir(directory);
    }
    command
}

fn launch_directory() -> Option<PathBuf> {
    env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_dir())
        .or_else(|| env::current_dir().ok().filter(|path| path.is_dir()))
}

fn spawn_command(command: Command) -> io::Result<(ChildController, Receiver<ProcessEvent>)> {
    spawn_command_with_signal(command, Arc::new(send_signal))
}

fn spawn_command_with_signal(
    command: Command,
    signal: SignalSender,
) -> io::Result<(ChildController, Receiver<ProcessEvent>)> {
    spawn_command_with_controls(command, signal, Arc::new(|child: &mut Child| child.kill()))
}

fn spawn_command_with_controls(
    mut command: Command,
    signal: SignalSender,
    killer: ChildKiller,
) -> io::Result<(ChildController, Receiver<ProcessEvent>)> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("child stdout was not piped"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("child stderr was not piped"))?;
    let (internal_sender, internal_receiver) = mpsc::sync_channel(PROCESS_CHANNEL_CAPACITY);
    let (event_sender, event_receiver) = mpsc::sync_channel(PROCESS_CHANNEL_CAPACITY);
    let (command_sender, command_receiver) = mpsc::channel();
    let output_dropped = Arc::new(AtomicBool::new(false));

    spawn_stdout_reader(stdout, internal_sender.clone(), Arc::clone(&output_dropped));
    spawn_stderr_reader(stderr, internal_sender, Arc::clone(&output_dropped));
    thread::spawn(move || {
        supervise(
            child,
            internal_receiver,
            command_receiver,
            event_sender,
            signal,
            killer,
            output_dropped,
        )
    });

    Ok((
        ChildController {
            commands: command_sender,
        },
        event_receiver,
    ))
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
    sender: &SyncSender<InternalEvent>,
    event: InternalEvent,
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

fn preserves_protocol_event(event: &ProcessEvent) -> bool {
    matches!(
        event,
        ProcessEvent::Stdout(
            CliEvent::Lifecycle { .. }
                | CliEvent::TranscriptionChunk { .. }
                | CliEvent::TranscriptionCompleted { .. }
                | CliEvent::SummaryCompleted { .. }
                | CliEvent::Error { .. }
        ) | ProcessEvent::ProtocolError(_)
    )
}

fn spawn_stdout_reader(
    stdout: impl io::Read + Send + 'static,
    sender: SyncSender<InternalEvent>,
    output_dropped: Arc<AtomicBool>,
) {
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        loop {
            let event = match read_bounded_line(&mut reader, MAX_PROTOCOL_LINE_BYTES) {
                Ok(Some(Ok(line))) => match parse_line(&line) {
                    Ok(event) => ProcessEvent::Stdout(event),
                    Err(error) => ProcessEvent::ProtocolError(error),
                },
                Ok(Some(Err(()))) => ProcessEvent::ProtocolError(PROTOCOL_TRUNCATED.into()),
                Ok(None) => break,
                Err(error) => {
                    let event =
                        ProcessEvent::ProtocolError(format!("Could not read CLI stdout: {error}"));
                    let _ =
                        send_internal(&sender, InternalEvent::Output(event), &output_dropped, true);
                    break;
                }
            };
            let preserve = preserves_protocol_event(&event);
            if !send_internal(
                &sender,
                InternalEvent::Output(event),
                &output_dropped,
                preserve,
            ) {
                return;
            }
        }
        let _ = sender.send(InternalEvent::StdoutClosed);
    });
}

fn spawn_stderr_reader(
    stderr: impl io::Read + Send + 'static,
    sender: SyncSender<InternalEvent>,
    output_dropped: Arc<AtomicBool>,
) {
    thread::spawn(move || {
        let mut reader = BufReader::new(stderr);
        loop {
            let event = match read_bounded_line(&mut reader, MAX_DIAGNOSTIC_LINE_BYTES) {
                Ok(Some(Ok(line))) => ProcessEvent::Stderr(line),
                Ok(Some(Err(()))) => ProcessEvent::Stderr(DIAGNOSTIC_TRUNCATED.into()),
                Ok(None) => break,
                Err(error) => {
                    let event = ProcessEvent::Stderr(format!("Could not read CLI stderr: {error}"));
                    let _ = send_internal(
                        &sender,
                        InternalEvent::Output(event),
                        &output_dropped,
                        false,
                    );
                    break;
                }
            };
            if !send_internal(
                &sender,
                InternalEvent::Output(event),
                &output_dropped,
                false,
            ) {
                return;
            }
        }
        let _ = sender.send(InternalEvent::StderrClosed);
    });
}

fn forward_event(
    events: &SyncSender<ProcessEvent>,
    event: ProcessEvent,
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
    events: &SyncSender<ProcessEvent>,
    output_dropped: &AtomicBool,
    force: bool,
) {
    if !output_dropped.swap(false, Ordering::AcqRel) {
        return;
    }
    let event = ProcessEvent::Stderr(OUTPUT_DROPPED.into());
    let sent = if force {
        events.send(event).is_ok()
    } else {
        events.try_send(event).is_ok()
    };
    if !sent && !force {
        output_dropped.store(true, Ordering::Release);
    }
}

enum ProcessCommand {
    Interrupt,
    Terminate,
}

enum InternalEvent {
    Output(ProcessEvent),
    StdoutClosed,
    StderrClosed,
}

fn supervise(
    mut child: Child,
    output: Receiver<InternalEvent>,
    commands: Receiver<ProcessCommand>,
    events: SyncSender<ProcessEvent>,
    signal: SignalSender,
    killer: ChildKiller,
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
                Ok(ProcessCommand::Interrupt) => {
                    let event = match signal(child.id(), libc::SIGINT) {
                        Ok(()) => ProcessEvent::InterruptDelivered,
                        Err(error) => ProcessEvent::InterruptFailed(error.to_string()),
                    };
                    let _ = events.send(event);
                }
                Ok(ProcessCommand::Terminate) => {
                    let event = match killer(&mut child) {
                        Ok(()) => ProcessEvent::TerminationDelivered,
                        Err(error) => ProcessEvent::TerminationFailed(error.to_string()),
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
                Ok(status) => ProcessEvent::Exited(status),
                Err(error) => ProcessEvent::WaitFailed(error),
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
            Ok(InternalEvent::Output(event)) => {
                let preserve = preserves_protocol_event(&event);
                if !forward_event(&events, event, &output_dropped, preserve) {
                    let _ = child.kill();
                    let _ = child.wait();
                    return;
                }
            }
            Ok(InternalEvent::StdoutClosed) => stdout_closed = true,
            Ok(InternalEvent::StderrClosed) => stderr_closed = true,
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

fn send_signal(pid: u32, signal: libc::c_int) -> io::Result<()> {
    let result = unsafe { libc::kill(pid as libc::pid_t, signal) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::atomic::{AtomicUsize, Ordering},
        time::Duration,
    };

    use super::*;

    fn receive_exit(receiver: &Receiver<ProcessEvent>) -> ExitStatus {
        loop {
            match receiver.recv_timeout(Duration::from_secs(5)).unwrap() {
                ProcessEvent::Exited(status) => return status,
                ProcessEvent::WaitFailed(error) => panic!("{error}"),
                _ => {}
            }
        }
    }

    #[test]
    fn resolves_sibling_cli() {
        assert_eq!(
            cli_path_for(Path::new("/tmp/target/debug/meetlite-gui")),
            Some(PathBuf::from("/tmp/target/debug/meetlite"))
        );
    }

    #[test]
    fn resolves_macos_bundle_resource_cli() {
        assert_eq!(
            cli_path_for(Path::new(
                "/Applications/Meetlite.app/Contents/MacOS/meetlite-gui"
            )),
            Some(PathBuf::from(
                "/Applications/Meetlite.app/Contents/Resources/meetlite"
            ))
        );
    }

    #[test]
    fn builds_exact_cli_command() {
        let command = cli_command(PathBuf::from("/tmp/meetlite"));
        assert_eq!(command.get_program(), "/tmp/meetlite");
        assert_eq!(command.get_args().collect::<Vec<_>>(), ["--json", "start"]);
        assert_eq!(command.get_current_dir(), launch_directory().as_deref());
    }

    #[test]
    fn reports_missing_program_at_launch() {
        let command = Command::new("/path/that/does/not/exist/meetlite");
        assert!(spawn_command(command).is_err());
    }

    #[test]
    fn child_stdin_is_null() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "if read value; then exit 1; fi"]);
        let (_controller, receiver) = spawn_command(command).unwrap();
        loop {
            match receiver.recv_timeout(Duration::from_secs(5)).unwrap() {
                ProcessEvent::Exited(status) => {
                    assert!(status.success());
                    break;
                }
                ProcessEvent::WaitFailed(error) => panic!("{error}"),
                _ => {}
            }
        }
    }

    #[test]
    fn oversized_output_line_is_discarded_without_echoing_and_next_line_is_parsed() {
        let secret = "oversized-secret";
        let mut bytes = secret
            .repeat(MAX_PROTOCOL_LINE_BYTES / secret.len() + 2)
            .into_bytes();
        bytes.push(b'\n');
        bytes.extend_from_slice(b"{\"type\":\"summary_delta\",\"text\":\"next\"}\n");
        let (sender, receiver) = mpsc::sync_channel(4);
        spawn_stdout_reader(
            io::Cursor::new(bytes),
            sender,
            Arc::new(AtomicBool::new(false)),
        );

        assert!(matches!(
            receiver.recv_timeout(Duration::from_secs(2)).unwrap(),
            InternalEvent::Output(ProcessEvent::ProtocolError(message))
                if message == PROTOCOL_TRUNCATED && !message.contains(secret)
        ));
        assert!(matches!(
            receiver.recv_timeout(Duration::from_secs(2)).unwrap(),
            InternalEvent::Output(ProcessEvent::Stdout(CliEvent::SummaryDelta { text }))
                if text == "next"
        ));
    }

    #[test]
    fn forwards_final_stdout_before_exit() {
        let mut command = Command::new("/bin/sh");
        command.args([
            "-c",
            "printf '%s\\n' '{\"type\":\"summary_delta\",\"text\":\"last\"}'; printf '%s\\n' diagnostic >&2",
        ]);
        let (_controller, receiver) = spawn_command(command).unwrap();
        let mut events = Vec::new();
        loop {
            let event = receiver.recv_timeout(Duration::from_secs(5)).unwrap();
            let exited = matches!(event, ProcessEvent::Exited(_));
            events.push(event);
            if exited {
                break;
            }
        }

        let stdout = events
            .iter()
            .position(|event| matches!(event, ProcessEvent::Stdout(CliEvent::SummaryDelta { .. })))
            .unwrap();
        let exit = events
            .iter()
            .position(|event| matches!(event, ProcessEvent::Exited(_)))
            .unwrap();
        assert!(stdout < exit);
        assert!(events
            .iter()
            .any(|event| matches!(event, ProcessEvent::Stderr(line) if line == "diagnostic")));
    }

    #[test]
    fn exited_child_drains_delayed_inherited_pipe_output_with_a_bound() {
        let mut command = Command::new("/bin/sh");
        command.args([
            "-c",
            "{ sleep 0.05; printf '%s\\n' '{\"type\":\"summary_delta\",\"text\":\"delayed\"}'; sleep 5; } & exit 0",
        ]);
        let started = Instant::now();
        let (_controller, receiver) = spawn_command(command).unwrap();
        let mut events = Vec::new();
        loop {
            let event = receiver.recv_timeout(Duration::from_secs(2)).unwrap();
            let exited = matches!(event, ProcessEvent::Exited(_));
            events.push(event);
            if exited {
                break;
            }
        }

        assert!(started.elapsed() < Duration::from_secs(2));
        let output = events
            .iter()
            .position(|event| matches!(event, ProcessEvent::Stdout(CliEvent::SummaryDelta { text }) if text == "delayed"))
            .unwrap();
        let exit = events
            .iter()
            .position(|event| matches!(event, ProcessEvent::Exited(_)))
            .unwrap();
        assert!(output < exit);
    }

    #[test]
    fn reports_actual_interrupt_delivery_success() {
        let calls = Arc::new(AtomicUsize::new(0));
        let signal_calls = Arc::clone(&calls);
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "sleep 30"]);
        let (controller, receiver) = spawn_command_with_signal(
            command,
            Arc::new(move |_pid, signal| {
                assert_eq!(signal, libc::SIGINT);
                signal_calls.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }),
        )
        .unwrap();

        controller.interrupt().unwrap();
        assert!(matches!(
            receiver.recv_timeout(Duration::from_secs(5)).unwrap(),
            ProcessEvent::InterruptDelivered
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        controller.terminate().unwrap();
        assert!(!receive_exit(&receiver).success());
    }

    #[test]
    fn reports_actual_interrupt_delivery_failure() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "sleep 30"]);
        let (controller, receiver) = spawn_command_with_signal(
            command,
            Arc::new(|_pid, _signal| Err(io::Error::other("signal refused"))),
        )
        .unwrap();

        controller.interrupt().unwrap();
        assert!(matches!(
            receiver.recv_timeout(Duration::from_secs(5)).unwrap(),
            ProcessEvent::InterruptFailed(error) if error == "signal refused"
        ));
        controller.terminate().unwrap();
        receive_exit(&receiver);
    }

    #[test]
    fn acknowledges_three_interrupt_actions_individually() {
        let calls = Arc::new(AtomicUsize::new(0));
        let signal_calls = Arc::clone(&calls);
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "sleep 30"]);
        let (controller, receiver) = spawn_command_with_signal(
            command,
            Arc::new(move |_pid, _signal| {
                signal_calls.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }),
        )
        .unwrap();

        for _ in 0..3 {
            controller.interrupt().unwrap();
            assert!(matches!(
                receiver.recv_timeout(Duration::from_secs(5)).unwrap(),
                ProcessEvent::InterruptDelivered
            ));
        }
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        controller.terminate().unwrap();
        receive_exit(&receiver);
    }

    #[test]
    fn fake_child_forwards_scripted_lifecycle_before_natural_completion() {
        let mut command = Command::new("/bin/sh");
        command.args([
            "-c",
            "printf '%s\\n' '{\"type\":\"lifecycle\",\"phase\":\"recording_started\",\"output_dir\":\"recording\"}' '{\"type\":\"lifecycle\",\"phase\":\"recording_stopped\",\"output_dir\":\"recording\"}' '{\"type\":\"lifecycle\",\"phase\":\"processing_started\",\"output_dir\":\"recording\"}' '{\"type\":\"lifecycle\",\"phase\":\"summarizing_started\",\"transcript_path\":\"transcript.json\"}' '{\"type\":\"summary_completed\",\"summary_path\":\"summary.md\",\"model\":\"test\",\"summary\":\"done\"}'",
        ]);
        let (_controller, receiver) = spawn_command(command).unwrap();
        let mut phases = Vec::new();
        let mut completed = false;
        loop {
            match receiver.recv_timeout(Duration::from_secs(5)).unwrap() {
                ProcessEvent::Stdout(CliEvent::Lifecycle { phase, .. }) => phases.push(phase),
                ProcessEvent::Stdout(CliEvent::SummaryCompleted { .. }) => completed = true,
                ProcessEvent::Exited(status) => {
                    assert!(status.success());
                    break;
                }
                ProcessEvent::WaitFailed(error) => panic!("{error}"),
                _ => {}
            }
        }
        assert_eq!(
            phases,
            [
                super::super::events::LifecyclePhase::RecordingStarted,
                super::super::events::LifecyclePhase::RecordingStopped,
                super::super::events::LifecyclePhase::ProcessingStarted,
                super::super::events::LifecyclePhase::SummarizingStarted,
            ]
        );
        assert!(completed);
    }

    #[test]
    fn fake_child_reports_nonzero_crash() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "exit 17"]);
        let (_controller, receiver) = spawn_command(command).unwrap();
        assert_eq!(receive_exit(&receiver).code(), Some(17));
    }

    #[test]
    fn terminate_hard_kills_fake_child_once_requested() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "sleep 30"]);
        let (controller, receiver) = spawn_command(command).unwrap();
        controller.terminate().unwrap();
        assert!(matches!(
            receiver.recv_timeout(Duration::from_secs(5)).unwrap(),
            ProcessEvent::TerminationDelivered
        ));
        assert!(!receive_exit(&receiver).success());
    }

    #[test]
    fn reports_actual_termination_failure_and_remains_controllable() {
        let calls = Arc::new(AtomicUsize::new(0));
        let killer_calls = Arc::clone(&calls);
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "sleep 30"]);
        let (controller, receiver) = spawn_command_with_controls(
            command,
            Arc::new(send_signal),
            Arc::new(move |_child| {
                killer_calls.fetch_add(1, Ordering::SeqCst);
                Err(io::Error::other("kill refused"))
            }),
        )
        .unwrap();

        for expected in 1..=2 {
            controller.terminate().unwrap();
            assert!(matches!(
                receiver.recv_timeout(Duration::from_secs(5)).unwrap(),
                ProcessEvent::TerminationFailed(error) if error == "kill refused"
            ));
            assert_eq!(calls.load(Ordering::SeqCst), expected);
        }
        drop(controller);
        receive_exit(&receiver);
    }

    #[test]
    fn dropping_controller_terminates_child() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "sleep 30"]);
        let (controller, receiver) = spawn_command(command).unwrap();
        drop(controller);
        assert!(!receive_exit(&receiver).success());
    }
}
