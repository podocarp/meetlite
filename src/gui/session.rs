#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum SessionState {
    SetupRequired,
    #[default]
    Ready,
    Recording,
    Processing,
    Summarizing,
    Terminating,
    Complete,
    Stopped,
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SessionEvent {
    SetupRequired,
    SetupCompleted,
    PrimaryControlPressed,
    ChildLaunchFailed,
    RecordingStarted,
    RecordingStopped,
    ProcessingStarted,
    SummarizingStarted,
    TranscriptionCompleted,
    SummaryCompleted,
    SignalDelivered,
    SignalDeliveryFailed,
    TerminationDelivered,
    TerminationFailed,
    ErrorReceived,
    ChildExited { success: bool },
    ChildWaitFailed,
    CancellationTimedOut,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SessionEffect {
    OpenSetup,
    SpawnChild,
    SendInterrupt,
    StartCancellationGrace,
    TerminateChild,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ControlVisual {
    Button(&'static str),
    Record,
    Spinner,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ControlSpec {
    pub(crate) visual: ControlVisual,
    pub(crate) tooltip: Option<&'static str>,
    pub(crate) enabled: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Session {
    state: SessionState,
    action_pending: bool,
    action_accepted: bool,
    completion_observed: bool,
    transcription_completed: bool,
    summary_enabled: bool,
    error_observed: bool,
    summary_stop_accepted: bool,
    termination_requested: bool,
}

impl Default for Session {
    fn default() -> Self {
        Self {
            state: SessionState::Ready,
            action_pending: false,
            action_accepted: false,
            completion_observed: false,
            transcription_completed: false,
            summary_enabled: true,
            error_observed: false,
            summary_stop_accepted: false,
            termination_requested: false,
        }
    }
}

impl Session {
    pub(crate) fn state(self) -> SessionState {
        self.state
    }

    #[cfg(test)]
    pub(crate) fn action_pending(self) -> bool {
        self.action_pending
    }

    pub(crate) fn error_observed(self) -> bool {
        self.error_observed
    }

    pub(crate) fn set_summary_enabled(&mut self, enabled: bool) {
        self.summary_enabled = enabled;
    }

    pub(crate) fn observe_recording_summary_enabled(&mut self, enabled: Option<bool>) {
        if let Some(enabled) = enabled {
            self.summary_enabled = enabled;
        }
    }

    pub(crate) fn control_spec(self) -> ControlSpec {
        let enabled = !self.action_pending && !self.action_accepted;
        match self.state {
            SessionState::SetupRequired => ControlSpec {
                visual: ControlVisual::Button("Open setup"),
                tooltip: None,
                enabled,
            },
            SessionState::Ready => ControlSpec {
                visual: ControlVisual::Button("Start"),
                tooltip: None,
                enabled,
            },
            SessionState::Recording => ControlSpec {
                visual: ControlVisual::Record,
                tooltip: Some("Stop recording"),
                enabled,
            },
            SessionState::Processing => ControlSpec {
                visual: ControlVisual::Spinner,
                tooltip: Some("Stop processing and summarize partial transcript"),
                enabled,
            },
            SessionState::Summarizing => ControlSpec {
                visual: ControlVisual::Spinner,
                tooltip: Some("Stop summary"),
                enabled,
            },
            SessionState::Terminating if self.action_pending => ControlSpec {
                visual: ControlVisual::Spinner,
                tooltip: Some("Terminating Meetlite CLI"),
                enabled: false,
            },
            SessionState::Terminating => ControlSpec {
                visual: ControlVisual::Button("Retry stop"),
                tooltip: None,
                enabled: true,
            },
            SessionState::Complete | SessionState::Stopped | SessionState::Failed => ControlSpec {
                visual: ControlVisual::Button("Reset"),
                tooltip: None,
                enabled: true,
            },
        }
    }

    pub(crate) fn reduce(&mut self, event: SessionEvent) -> Option<SessionEffect> {
        match event {
            SessionEvent::SetupRequired => {
                self.reset(SessionState::SetupRequired);
                None
            }
            SessionEvent::SetupCompleted if self.state == SessionState::SetupRequired => {
                self.reset(SessionState::Ready);
                None
            }
            SessionEvent::PrimaryControlPressed => self.primary_control_pressed(),
            SessionEvent::ChildLaunchFailed if self.state == SessionState::Ready => {
                self.reset(SessionState::Failed);
                None
            }
            SessionEvent::RecordingStarted if self.state == SessionState::Ready => {
                self.enter_phase(SessionState::Recording);
                None
            }
            SessionEvent::RecordingStopped
                if matches!(
                    self.state,
                    SessionState::Recording | SessionState::Processing
                ) =>
            {
                self.enter_phase(SessionState::Processing);
                None
            }
            SessionEvent::ProcessingStarted
                if matches!(
                    self.state,
                    SessionState::Recording | SessionState::Processing
                ) =>
            {
                self.enter_phase(SessionState::Processing);
                None
            }
            SessionEvent::SummarizingStarted
                if matches!(
                    self.state,
                    SessionState::Processing | SessionState::Summarizing
                ) =>
            {
                self.enter_phase(SessionState::Summarizing);
                None
            }
            SessionEvent::TranscriptionCompleted
                if matches!(
                    self.state,
                    SessionState::Processing | SessionState::Summarizing
                ) =>
            {
                self.transcription_completed = true;
                if !self.summary_enabled {
                    self.completion_observed = true;
                }
                None
            }
            SessionEvent::SummaryCompleted if self.state == SessionState::Summarizing => {
                self.completion_observed = true;
                None
            }
            SessionEvent::SignalDelivered if self.action_pending => {
                self.action_pending = false;
                self.action_accepted = true;
                if self.state == SessionState::Summarizing {
                    self.summary_stop_accepted = true;
                    Some(SessionEffect::StartCancellationGrace)
                } else {
                    None
                }
            }
            SessionEvent::SignalDeliveryFailed if self.action_pending => {
                self.action_pending = false;
                None
            }
            SessionEvent::TerminationDelivered
                if self.state == SessionState::Terminating && self.action_pending =>
            {
                self.state = SessionState::Stopped;
                self.action_pending = false;
                None
            }
            SessionEvent::TerminationFailed
                if self.state == SessionState::Terminating && self.action_pending =>
            {
                self.action_pending = false;
                self.termination_requested = false;
                None
            }
            SessionEvent::ErrorReceived
                if !matches!(
                    self.state,
                    SessionState::Terminating
                        | SessionState::Complete
                        | SessionState::Stopped
                        | SessionState::Failed
                ) =>
            {
                self.state = SessionState::Failed;
                self.action_pending = false;
                self.error_observed = true;
                None
            }
            SessionEvent::ChildExited { success } => {
                self.child_exited(success);
                None
            }
            SessionEvent::ChildWaitFailed => {
                self.state = SessionState::Failed;
                self.action_pending = false;
                None
            }
            SessionEvent::CancellationTimedOut
                if self.state == SessionState::Summarizing
                    && self.summary_stop_accepted
                    && !self.termination_requested =>
            {
                self.state = SessionState::Terminating;
                self.action_pending = true;
                self.action_accepted = false;
                self.termination_requested = true;
                Some(SessionEffect::TerminateChild)
            }
            _ => None,
        }
    }

    fn primary_control_pressed(&mut self) -> Option<SessionEffect> {
        if matches!(
            self.state,
            SessionState::Complete | SessionState::Stopped | SessionState::Failed
        ) {
            self.reset(SessionState::Ready);
            return None;
        }
        if self.action_pending || self.action_accepted {
            return None;
        }
        match self.state {
            SessionState::SetupRequired => Some(SessionEffect::OpenSetup),
            SessionState::Ready => {
                self.action_pending = true;
                Some(SessionEffect::SpawnChild)
            }
            SessionState::Recording | SessionState::Processing | SessionState::Summarizing => {
                self.action_pending = true;
                Some(SessionEffect::SendInterrupt)
            }
            SessionState::Terminating => {
                self.action_pending = true;
                self.termination_requested = true;
                Some(SessionEffect::TerminateChild)
            }
            SessionState::Complete | SessionState::Stopped | SessionState::Failed => unreachable!(),
        }
    }

    fn child_exited(&mut self, success: bool) {
        if self.state == SessionState::Stopped || self.state == SessionState::Failed {
            self.action_pending = false;
        } else if success && self.completion_observed {
            self.reset(SessionState::Complete);
        } else if self.summary_stop_accepted {
            self.state = SessionState::Stopped;
            self.action_pending = false;
        } else {
            self.state = SessionState::Failed;
            self.action_pending = false;
        }
    }

    fn enter_phase(&mut self, state: SessionState) {
        if self.state != state {
            self.state = state;
            self.action_pending = false;
            self.action_accepted = false;
        }
    }

    fn reset(&mut self, state: SessionState) {
        let summary_enabled = self.summary_enabled;
        *self = Self {
            state,
            summary_enabled,
            ..Self::default()
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn advance_to_recording(session: &mut Session) {
        assert_eq!(
            session.reduce(SessionEvent::PrimaryControlPressed),
            Some(SessionEffect::SpawnChild)
        );
        assert!(session.action_pending());
        session.reduce(SessionEvent::RecordingStarted);
        assert_eq!(session.state(), SessionState::Recording);
    }

    fn advance_to_processing(session: &mut Session) {
        advance_to_recording(session);
        session.reduce(SessionEvent::RecordingStopped);
        session.reduce(SessionEvent::ProcessingStarted);
        assert_eq!(session.state(), SessionState::Processing);
    }

    fn advance_to_summarizing(session: &mut Session) {
        advance_to_processing(session);
        session.reduce(SessionEvent::SummarizingStarted);
        assert_eq!(session.state(), SessionState::Summarizing);
    }

    fn accept_stop(session: &mut Session) -> Option<SessionEffect> {
        assert_eq!(
            session.reduce(SessionEvent::PrimaryControlPressed),
            Some(SessionEffect::SendInterrupt)
        );
        session.reduce(SessionEvent::SignalDelivered)
    }

    #[test]
    fn control_specs_match_every_state() {
        let cases = [
            (
                SessionState::SetupRequired,
                ControlVisual::Button("Open setup"),
                None,
            ),
            (SessionState::Ready, ControlVisual::Button("Start"), None),
            (
                SessionState::Recording,
                ControlVisual::Record,
                Some("Stop recording"),
            ),
            (
                SessionState::Processing,
                ControlVisual::Spinner,
                Some("Stop processing and summarize partial transcript"),
            ),
            (
                SessionState::Summarizing,
                ControlVisual::Spinner,
                Some("Stop summary"),
            ),
            (
                SessionState::Terminating,
                ControlVisual::Button("Retry stop"),
                None,
            ),
            (SessionState::Complete, ControlVisual::Button("Reset"), None),
            (SessionState::Stopped, ControlVisual::Button("Reset"), None),
            (SessionState::Failed, ControlVisual::Button("Reset"), None),
        ];

        for (state, visual, tooltip) in cases {
            let session = Session {
                state,
                ..Session::default()
            };
            assert_eq!(
                session.control_spec(),
                ControlSpec {
                    visual,
                    tooltip,
                    enabled: true,
                }
            );
        }
    }

    #[test]
    fn setup_opens_and_returns_to_ready() {
        let mut session = Session::default();
        session.reduce(SessionEvent::SetupRequired);
        assert_eq!(session.state(), SessionState::SetupRequired);
        assert_eq!(
            session.reduce(SessionEvent::PrimaryControlPressed),
            Some(SessionEffect::OpenSetup)
        );
        session.reduce(SessionEvent::SetupCompleted);
        assert_eq!(session.state(), SessionState::Ready);
    }

    #[test]
    fn each_phase_action_waits_for_actual_delivery() {
        let mut session = Session::default();
        advance_to_recording(&mut session);

        for (next, state) in [
            (SessionEvent::RecordingStopped, SessionState::Processing),
            (SessionEvent::SummarizingStarted, SessionState::Summarizing),
        ] {
            assert_eq!(
                session.reduce(SessionEvent::PrimaryControlPressed),
                Some(SessionEffect::SendInterrupt)
            );
            assert!(!session.control_spec().enabled);
            assert_eq!(session.reduce(SessionEvent::PrimaryControlPressed), None);
            assert_eq!(session.reduce(SessionEvent::SignalDelivered), None);
            assert!(!session.control_spec().enabled);
            assert_eq!(session.reduce(SessionEvent::PrimaryControlPressed), None);
            session.reduce(next);
            assert_eq!(session.state(), state);
            assert!(session.control_spec().enabled);
        }

        assert_eq!(
            session.reduce(SessionEvent::PrimaryControlPressed),
            Some(SessionEffect::SendInterrupt)
        );
        assert_eq!(
            session.reduce(SessionEvent::SignalDelivered),
            Some(SessionEffect::StartCancellationGrace)
        );
        assert!(!session.control_spec().enabled);
        assert_eq!(session.reduce(SessionEvent::PrimaryControlPressed), None);
    }

    #[test]
    fn failed_signal_delivery_allows_retry_without_failing_session() {
        let mut session = Session::default();
        advance_to_recording(&mut session);
        session.reduce(SessionEvent::PrimaryControlPressed);
        session.reduce(SessionEvent::SignalDeliveryFailed);
        assert_eq!(session.state(), SessionState::Recording);
        assert!(session.control_spec().enabled);
        assert_eq!(
            session.reduce(SessionEvent::PrimaryControlPressed),
            Some(SessionEffect::SendInterrupt)
        );
    }

    #[test]
    fn summary_grace_starts_only_after_delivery() {
        let mut session = Session::default();
        advance_to_summarizing(&mut session);
        session.reduce(SessionEvent::PrimaryControlPressed);
        assert_eq!(session.reduce(SessionEvent::CancellationTimedOut), None);
        assert_eq!(session.state(), SessionState::Summarizing);
        assert_eq!(
            session.reduce(SessionEvent::SignalDelivered),
            Some(SessionEffect::StartCancellationGrace)
        );
    }

    #[test]
    fn summary_timeout_waits_for_termination_acknowledgement() {
        let mut session = Session::default();
        advance_to_summarizing(&mut session);
        accept_stop(&mut session);
        assert_eq!(
            session.reduce(SessionEvent::CancellationTimedOut),
            Some(SessionEffect::TerminateChild)
        );
        assert_eq!(session.state(), SessionState::Terminating);
        assert!(!session.control_spec().enabled);
        assert_eq!(session.reduce(SessionEvent::CancellationTimedOut), None);
        session.reduce(SessionEvent::TerminationDelivered);
        assert_eq!(session.state(), SessionState::Stopped);
        session.reduce(SessionEvent::ErrorReceived);
        session.reduce(SessionEvent::SummaryCompleted);
        session.reduce(SessionEvent::ChildExited { success: false });
        assert_eq!(session.state(), SessionState::Stopped);
    }

    #[test]
    fn failed_termination_stays_truthful_and_allows_retry() {
        let mut session = Session::default();
        advance_to_summarizing(&mut session);
        accept_stop(&mut session);
        session.reduce(SessionEvent::CancellationTimedOut);
        session.reduce(SessionEvent::TerminationFailed);

        assert_eq!(session.state(), SessionState::Terminating);
        assert_eq!(
            session.control_spec(),
            ControlSpec {
                visual: ControlVisual::Button("Retry stop"),
                tooltip: None,
                enabled: true,
            }
        );
        assert_eq!(
            session.reduce(SessionEvent::PrimaryControlPressed),
            Some(SessionEffect::TerminateChild)
        );
    }

    #[test]
    fn exit_while_termination_is_pending_stops_the_session() {
        let mut session = Session::default();
        advance_to_summarizing(&mut session);
        accept_stop(&mut session);
        session.reduce(SessionEvent::CancellationTimedOut);
        session.reduce(SessionEvent::ChildExited { success: false });
        assert_eq!(session.state(), SessionState::Stopped);
    }

    #[test]
    fn wait_failure_during_termination_is_failed_not_stopped() {
        let mut session = Session::default();
        advance_to_summarizing(&mut session);
        accept_stop(&mut session);
        session.reduce(SessionEvent::CancellationTimedOut);
        session.reduce(SessionEvent::ChildWaitFailed);
        assert_eq!(session.state(), SessionState::Failed);
    }

    #[test]
    fn accepted_summary_stop_classifies_success_and_failure_as_stopped() {
        for success in [true, false] {
            let mut session = Session::default();
            advance_to_summarizing(&mut session);
            accept_stop(&mut session);
            session.reduce(SessionEvent::ChildExited { success });
            assert_eq!(session.state(), SessionState::Stopped);
        }
    }

    #[test]
    fn completed_summary_wins_a_race_with_an_accepted_stop() {
        let mut session = Session::default();
        advance_to_summarizing(&mut session);
        accept_stop(&mut session);
        session.reduce(SessionEvent::SummaryCompleted);
        session.reduce(SessionEvent::ChildExited { success: true });
        assert_eq!(session.state(), SessionState::Complete);
    }

    #[test]
    fn no_summary_completion_requires_transcription_and_successful_exit() {
        let mut complete = Session::default();
        complete.set_summary_enabled(false);
        advance_to_processing(&mut complete);
        complete.reduce(SessionEvent::TranscriptionCompleted);
        complete.reduce(SessionEvent::ChildExited { success: true });
        assert_eq!(complete.state(), SessionState::Complete);

        let mut missing_event = Session::default();
        missing_event.set_summary_enabled(false);
        advance_to_processing(&mut missing_event);
        missing_event.reduce(SessionEvent::ChildExited { success: true });
        assert_eq!(missing_event.state(), SessionState::Failed);

        let mut crash = Session::default();
        crash.set_summary_enabled(false);
        advance_to_processing(&mut crash);
        crash.reduce(SessionEvent::TranscriptionCompleted);
        crash.reduce(SessionEvent::ChildExited { success: false });
        assert_eq!(crash.state(), SessionState::Failed);
    }

    #[test]
    fn natural_completion_requires_final_event_and_successful_exit() {
        let mut complete = Session::default();
        advance_to_summarizing(&mut complete);
        complete.reduce(SessionEvent::SummaryCompleted);
        complete.reduce(SessionEvent::ChildExited { success: true });
        assert_eq!(complete.state(), SessionState::Complete);

        let mut missing_event = Session::default();
        advance_to_summarizing(&mut missing_event);
        missing_event.reduce(SessionEvent::ChildExited { success: true });
        assert_eq!(missing_event.state(), SessionState::Failed);

        let mut crash = Session::default();
        advance_to_summarizing(&mut crash);
        crash.reduce(SessionEvent::SummaryCompleted);
        crash.reduce(SessionEvent::ChildExited { success: false });
        assert_eq!(crash.state(), SessionState::Failed);
    }

    #[test]
    fn launch_protocol_and_phase_crashes_fail() {
        let mut launch_failure = Session::default();
        launch_failure.reduce(SessionEvent::PrimaryControlPressed);
        launch_failure.reduce(SessionEvent::ChildLaunchFailed);
        assert_eq!(launch_failure.state(), SessionState::Failed);

        let mut structured_error = Session::default();
        advance_to_recording(&mut structured_error);
        structured_error.reduce(SessionEvent::ErrorReceived);
        assert!(structured_error.error_observed());
        structured_error.reduce(SessionEvent::ChildExited { success: false });
        assert_eq!(structured_error.state(), SessionState::Failed);

        for phase in [
            SessionState::Recording,
            SessionState::Processing,
            SessionState::Summarizing,
        ] {
            let mut crash = Session::default();
            match phase {
                SessionState::Recording => advance_to_recording(&mut crash),
                SessionState::Processing => advance_to_processing(&mut crash),
                SessionState::Summarizing => advance_to_summarizing(&mut crash),
                _ => unreachable!(),
            }
            crash.reduce(SessionEvent::ChildExited { success: false });
            assert_eq!(crash.state(), SessionState::Failed);
        }
    }

    #[test]
    fn duplicate_lifecycle_events_do_not_reenable_an_accepted_action() {
        let mut session = Session::default();
        advance_to_processing(&mut session);
        accept_stop(&mut session);
        session.reduce(SessionEvent::ProcessingStarted);
        assert!(!session.control_spec().enabled);
        assert_eq!(session.reduce(SessionEvent::PrimaryControlPressed), None);
    }

    #[test]
    fn reset_clears_every_terminal_session_field() {
        for terminal in [
            SessionState::Complete,
            SessionState::Stopped,
            SessionState::Failed,
        ] {
            let mut session = Session {
                state: terminal,
                action_pending: true,
                action_accepted: true,
                completion_observed: true,
                transcription_completed: true,
                summary_enabled: true,
                error_observed: true,
                summary_stop_accepted: true,
                termination_requested: true,
            };
            assert_eq!(session.reduce(SessionEvent::PrimaryControlPressed), None);
            assert_eq!(session, Session::default());
        }
    }
}
