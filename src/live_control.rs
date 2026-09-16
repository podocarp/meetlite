use std::{
    io::{self, IsTerminal, Read},
    process,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    thread,
};

use anyhow::{Context, Result};

#[derive(Clone)]
pub(crate) struct LiveControl {
    interrupts: Arc<AtomicUsize>,
    commit_boundary: Arc<Mutex<()>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LivePhase {
    Recording = 0,
    Transcription = 1,
    Summary = 2,
}

fn phase_interrupts(current: usize, phase: LivePhase) -> usize {
    current.max(phase as usize)
}

fn phase_stopped(interrupts: usize, phase: LivePhase) -> bool {
    interrupts > phase as usize
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LifecyclePhase {
    RecordingStarted,
    RecordingStopped,
    ProcessingStarted,
    SummarizingStarted,
}

impl LifecyclePhase {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::RecordingStarted => "recording_started",
            Self::RecordingStopped => "recording_stopped",
            Self::ProcessingStarted => "processing_started",
            Self::SummarizingStarted => "summarizing_started",
        }
    }

    #[cfg(test)]
    fn follows(self, previous: Self) -> bool {
        matches!(
            (previous, self),
            (Self::RecordingStarted, Self::RecordingStopped)
                | (Self::RecordingStopped, Self::ProcessingStarted)
                | (Self::ProcessingStarted, Self::SummarizingStarted)
        )
    }
}

impl LiveControl {
    pub(crate) fn install() -> Result<Self> {
        let interrupts = Arc::new(AtomicUsize::new(0));
        let commit_boundary = Arc::new(Mutex::new(()));
        let signal_interrupts = Arc::clone(&interrupts);
        let signal_commit_boundary = Arc::clone(&commit_boundary);
        ctrlc::set_handler(move || {
            let _boundary = signal_commit_boundary
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            signal_interrupts.fetch_add(1, Ordering::AcqRel);
        })
        .context("could not install Ctrl-C handler")?;

        if io::stdin().is_terminal() {
            thread::spawn(|| {
                let mut stdin = io::stdin().lock();
                let mut byte = [0; 1];
                loop {
                    match stdin.read(&mut byte) {
                        // Ctrl-D on an empty terminal input buffer is an EOF read.
                        Ok(0) => process::exit(0),
                        Ok(_) => {}
                        Err(_) => return,
                    }
                }
            });
        }

        Ok(Self {
            interrupts,
            commit_boundary,
        })
    }

    pub(crate) fn recording_stopped(&self) -> bool {
        self.phase_stopped(LivePhase::Recording)
    }

    pub(crate) fn transcription_stopped(&self) -> bool {
        self.phase_stopped(LivePhase::Transcription)
    }

    pub(crate) fn summary_stopped(&self) -> bool {
        self.phase_stopped(LivePhase::Summary)
    }

    pub(crate) fn begin_transcription(&self) {
        self.begin_phase(LivePhase::Transcription);
    }

    pub(crate) fn begin_summary(&self) {
        self.begin_phase(LivePhase::Summary);
    }

    pub(crate) fn stop_transcription(&self) {
        let _boundary = self
            .commit_boundary
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.interrupts
            .fetch_max(LivePhase::Summary as usize, Ordering::AcqRel);
    }

    pub(crate) fn at_commit_boundary<T>(&self, action: impl FnOnce() -> Result<T>) -> Result<T> {
        let _boundary = self
            .commit_boundary
            .lock()
            .map_err(|_| anyhow::anyhow!("live commit boundary was poisoned"))?;
        action()
    }

    fn begin_phase(&self, phase: LivePhase) {
        let _ = self
            .interrupts
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                Some(phase_interrupts(current, phase))
            });
    }

    fn phase_stopped(&self, phase: LivePhase) -> bool {
        phase_stopped(self.interrupts.load(Ordering::Acquire), phase)
    }

    #[cfg(test)]
    pub(crate) fn testing(interrupts: usize) -> Self {
        Self {
            interrupts: Arc::new(AtomicUsize::new(interrupts)),
            commit_boundary: Arc::new(Mutex::new(())),
        }
    }

    #[cfg(test)]
    pub(crate) fn test_interrupt(&self) {
        let _boundary = self
            .commit_boundary
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.interrupts.fetch_add(1, Ordering::AcqRel);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn control(interrupts: usize) -> LiveControl {
        LiveControl::testing(interrupts)
    }

    #[test]
    fn natural_phase_changes_preserve_the_next_ctrl_c_action() {
        let control = control(0);

        control.begin_transcription();
        assert!(!control.transcription_stopped());
        control.interrupts.fetch_add(1, Ordering::AcqRel);
        assert!(control.transcription_stopped());

        control.begin_summary();
        assert!(!control.summary_stopped());
        control.interrupts.fetch_add(1, Ordering::AcqRel);
        assert!(control.summary_stopped());
    }

    #[test]
    fn phase_helpers_never_discard_early_interrupts() {
        assert_eq!(phase_interrupts(0, LivePhase::Transcription), 1);
        assert_eq!(phase_interrupts(2, LivePhase::Transcription), 2);
        assert_eq!(phase_interrupts(3, LivePhase::Summary), 3);
        assert!(phase_stopped(2, LivePhase::Transcription));
        assert!(!phase_stopped(2, LivePhase::Summary));
        assert!(phase_stopped(3, LivePhase::Summary));
    }

    #[test]
    fn second_interrupt_cancels_transcription_but_allows_summary() {
        let control = control(2);

        assert!(control.transcription_stopped());
        control.begin_summary();
        assert!(!control.summary_stopped());

        control.interrupts.fetch_add(1, Ordering::AcqRel);
        assert!(control.summary_stopped());
    }

    #[test]
    fn lifecycle_phases_have_one_valid_order() {
        let phases = [
            LifecyclePhase::RecordingStarted,
            LifecyclePhase::RecordingStopped,
            LifecyclePhase::ProcessingStarted,
            LifecyclePhase::SummarizingStarted,
        ];

        assert!(phases.windows(2).all(|pair| pair[1].follows(pair[0])));
        assert!(!LifecyclePhase::SummarizingStarted.follows(LifecyclePhase::RecordingStopped));
    }
}
