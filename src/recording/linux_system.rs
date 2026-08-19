use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

use alsa::{
    pcm::{Access, Format, HwParams, PCM},
    Direction, ValueOr,
};
use anyhow::{Context, Result};
use crossbeam_channel::{bounded, Receiver};

use super::pulse_system::PulseAudioCapture;
use super::{AudioFrame, CaptureStatistics, SourceBuffer, SourceKind, SAMPLE_RATE, WINDOW_SAMPLES};

pub enum SystemAudioCapture {
    PulseAudio(PulseAudioCapture),
    Alsa(AlsaSystemAudioCapture),
}

impl SystemAudioCapture {
    pub fn start(alsa_device: Option<&str>) -> Result<Self> {
        match PulseAudioCapture::start() {
            Ok(capture) => {
                println!("Capturing system audio from the default PulseAudio monitor.");
                Ok(Self::PulseAudio(capture))
            }
            Err(pulse_error) => {
                let device = alsa_device.context(format!(
                    "could not connect to the default PulseAudio monitor source ({pulse_error}); configure recording.system_device with an ALSA PCM capture device such as hw:Loopback,1,0"
                ))?;
                AlsaSystemAudioCapture::start(device)
                    .with_context(|| format!("PulseAudio monitor capture failed: {pulse_error}"))
                    .map(|capture| {
                        println!("PulseAudio is unavailable; capturing system audio from ALSA device {device}.");
                        Self::Alsa(capture)
                    })
            }
        }
    }

    pub fn sample_rate(&self) -> u32 {
        match self {
            Self::PulseAudio(capture) => capture.sample_rate(),
            Self::Alsa(capture) => capture.sample_rate(),
        }
    }

    pub fn drain_into(&self, output: &mut SourceBuffer) {
        match self {
            Self::PulseAudio(capture) => capture.drain_into(output),
            Self::Alsa(capture) => capture.drain_into(output),
        }
    }

    pub fn stop(self, output: &mut SourceBuffer) -> CaptureStatistics {
        match self {
            Self::PulseAudio(capture) => capture.stop(output),
            Self::Alsa(capture) => capture.stop(output),
        }
    }
}

pub struct AlsaSystemAudioCapture {
    receiver: Receiver<AudioFrame>,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
    dropped_frames: Arc<AtomicU64>,
}

impl AlsaSystemAudioCapture {
    pub fn start(device_name: &str) -> Result<Self> {
        let pcm = PCM::new(device_name, Direction::Capture, false)
            .with_context(|| format!("could not open ALSA capture device {device_name:?}"))?;
        let parameters =
            HwParams::any(&pcm).context("could not create ALSA hardware parameters")?;
        parameters
            .set_channels(1)
            .context("could not configure ALSA capture channels")?;
        parameters
            .set_rate(SAMPLE_RATE, ValueOr::Nearest)
            .context("could not configure ALSA capture sample rate")?;
        parameters
            .set_format(Format::s16())
            .context("could not configure ALSA capture sample format")?;
        parameters
            .set_access(Access::RWInterleaved)
            .context("could not configure ALSA capture access mode")?;
        parameters
            .set_period_size(WINDOW_SAMPLES as i64, ValueOr::Nearest)
            .context("could not configure ALSA capture period size")?;
        pcm.hw_params(&parameters)
            .context("could not apply ALSA capture parameters")?;
        let sample_rate = parameters
            .get_rate()
            .context("could not read ALSA capture sample rate")?;
        if sample_rate != SAMPLE_RATE {
            anyhow::bail!(
                "ALSA capture device {device_name:?} selected {sample_rate} Hz instead of 48000 Hz"
            );
        }
        drop(parameters);

        let (sender, receiver) = bounded(128);
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let dropped_frames = Arc::new(AtomicU64::new(0));
        let thread_dropped_frames = Arc::clone(&dropped_frames);
        let thread = thread::spawn(move || {
            let input = match pcm.io_i16() {
                Ok(input) => input,
                Err(error) => {
                    eprintln!("could not access ALSA capture samples: {error}");
                    return;
                }
            };
            let mut samples = vec![0_i16; WINDOW_SAMPLES];
            while !thread_stop.load(Ordering::Acquire) {
                match input.readi(&mut samples) {
                    Ok(frames) => {
                        let samples = samples[..frames]
                            .iter()
                            .map(|sample| *sample as f32 / i16::MAX as f32)
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
                    Err(error) => {
                        eprintln!("ALSA system-audio stream error: {error}");
                        if let Err(error) = pcm.prepare() {
                            eprintln!("could not recover ALSA system-audio stream: {error}");
                            break;
                        }
                        thread::sleep(Duration::from_millis(5));
                    }
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
