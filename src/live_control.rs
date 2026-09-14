use std::{
    io::{self, IsTerminal, Read},
    process,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    thread,
};

use anyhow::{Context, Result};

#[derive(Clone)]
pub(crate) struct LiveControl {
    // 0 records, 1 transcribes, 2 summarizes, and 3 exits.
    interrupts: Arc<AtomicUsize>,
}

impl LiveControl {
    pub(crate) fn install() -> Result<Self> {
        let interrupts = Arc::new(AtomicUsize::new(0));
        let signal_interrupts = Arc::clone(&interrupts);
        ctrlc::set_handler(move || {
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

        Ok(Self { interrupts })
    }

    pub(crate) fn recording_stopped(&self) -> bool {
        self.interrupts.load(Ordering::Acquire) >= 1
    }

    pub(crate) fn transcription_stopped(&self) -> bool {
        self.interrupts.load(Ordering::Acquire) >= 2
    }

    pub(crate) fn summary_stopped(&self) -> bool {
        self.interrupts.load(Ordering::Acquire) >= 3
    }

    pub(crate) fn begin_transcription(&self) {
        let _ = self
            .interrupts
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire);
    }

    pub(crate) fn begin_summary(&self) {
        let _ = self
            .interrupts
            .compare_exchange(1, 2, Ordering::AcqRel, Ordering::Acquire);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_phase_changes_preserve_the_next_ctrl_c_action() {
        let control = LiveControl {
            interrupts: Arc::new(AtomicUsize::new(0)),
        };

        control.begin_transcription();
        assert!(!control.transcription_stopped());
        control.interrupts.fetch_add(1, Ordering::AcqRel);
        assert!(control.transcription_stopped());

        control.begin_summary();
        assert!(!control.summary_stopped());
        control.interrupts.fetch_add(1, Ordering::AcqRel);
        assert!(control.summary_stopped());
    }
}
