use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    thread,
    time::Instant,
};

use anyhow::{Context, Result};
use crossbeam_channel::{bounded, Receiver};
use libpulse_binding::{
    sample::{Format, Spec},
    stream::Direction,
};
use libpulse_simple_binding::Simple;

use super::{AudioFrame, CaptureStatistics, SourceBuffer, SourceKind, SAMPLE_RATE, WINDOW_SAMPLES};

const DEFAULT_MONITOR: &str = "@DEFAULT_MONITOR@";

pub struct PulseAudioCapture {
    receiver: Receiver<AudioFrame>,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
    dropped_frames: Arc<AtomicU64>,
}

impl PulseAudioCapture {
    pub fn start() -> Result<Self> {
        let specification = Spec {
            format: Format::S16le,
            rate: SAMPLE_RATE,
            channels: 1,
        };
        let stream = Simple::new(
            None,
            "meetlite",
            Direction::Record,
            Some(DEFAULT_MONITOR),
            "system audio",
            &specification,
            None,
            None,
        )
        .context("could not connect to the default PulseAudio monitor source")?;

        let (sender, receiver) = bounded(128);
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let dropped_frames = Arc::new(AtomicU64::new(0));
        let thread_dropped_frames = Arc::clone(&dropped_frames);
        let thread = thread::spawn(move || {
            let mut bytes = vec![0_u8; WINDOW_SAMPLES * std::mem::size_of::<i16>()];
            while !thread_stop.load(Ordering::Acquire) {
                if let Err(error) = stream.read(&mut bytes) {
                    if !thread_stop.load(Ordering::Acquire) {
                        eprintln!("PulseAudio system-audio stream error: {error}");
                    }
                    break;
                }
                let samples = bytes
                    .chunks_exact(2)
                    .map(|sample| {
                        i16::from_le_bytes([sample[0], sample[1]]) as f32 / i16::MAX as f32
                    })
                    .collect();
                if sender
                    .try_send(AudioFrame {
                        source: SourceKind::System,
                        captured_at: Instant::now(),
                        sample_rate: SAMPLE_RATE,
                        samples,
                    })
                    .is_err()
                {
                    thread_dropped_frames.fetch_add(1, Ordering::Relaxed);
                }
            }
        });

        Ok(Self {
            receiver,
            stop,
            thread: Some(thread),
            dropped_frames,
        })
    }

    pub fn sample_rate(&self) -> u32 {
        SAMPLE_RATE
    }

    pub fn drain_into(&self, output: &mut SourceBuffer) {
        while let Ok(frame) = self.receiver.try_recv() {
            output.push(frame);
        }
    }

    pub fn stop(mut self, output: &mut SourceBuffer) -> CaptureStatistics {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        self.drain_into(output);
        CaptureStatistics {
            dropped_callback_frames: self.dropped_frames.load(Ordering::Relaxed),
            dropped_buffered_frames: output.dropped_frames(),
        }
    }
}
