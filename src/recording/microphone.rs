use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Instant,
};

use anyhow::{bail, Context, Result};
use cpal::{
    traits::{DeviceTrait, HostTrait, StreamTrait},
    SampleFormat, Stream,
};
use crossbeam_channel::{bounded, Receiver};

use super::{AudioFrame, CaptureStatistics, SourceKind};

pub struct MicrophoneCapture {
    receiver: Receiver<AudioFrame>,
    stream: Option<Stream>,
    sample_rate: u32,
    dropped_frames: Arc<AtomicU64>,
}

impl MicrophoneCapture {
    pub fn start(device_name: Option<&str>) -> Result<Self> {
        let host = cpal::default_host();
        let device = match device_name {
            Some(name) => host
                .input_devices()
                .context("could not enumerate microphone devices")?
                .find(|device| device.name().ok().as_deref() == Some(name))
                .with_context(|| format!("could not find microphone device {name:?}"))?,
            None => host
                .default_input_device()
                .context("no default microphone device is available")?,
        };
        let config = device
            .default_input_config()
            .context("could not get microphone input configuration")?;
        let sample_rate = config.sample_rate().0;
        let channels = config.channels() as usize;
        let (sender, receiver) = bounded(128);
        let dropped_frames = Arc::new(AtomicU64::new(0));
        let error_callback = |error| eprintln!("microphone stream error: {error}");

        let stream = match config.sample_format() {
            SampleFormat::F32 => {
                let dropped_frames = Arc::clone(&dropped_frames);
                device.build_input_stream(
                    &config.into(),
                    move |samples: &[f32], _| {
                        send_mono(
                            &sender,
                            &dropped_frames,
                            samples,
                            channels,
                            sample_rate,
                            |sample| sample,
                        )
                    },
                    error_callback,
                    None,
                )
            }
            SampleFormat::I16 => {
                let dropped_frames = Arc::clone(&dropped_frames);
                device.build_input_stream(
                    &config.into(),
                    move |samples: &[i16], _| {
                        send_mono(
                            &sender,
                            &dropped_frames,
                            samples,
                            channels,
                            sample_rate,
                            |sample| sample as f32 / i16::MAX as f32,
                        )
                    },
                    error_callback,
                    None,
                )
            }
            SampleFormat::U16 => {
                let dropped_frames = Arc::clone(&dropped_frames);
                device.build_input_stream(
                    &config.into(),
                    move |samples: &[u16], _| {
                        send_mono(
                            &sender,
                            &dropped_frames,
                            samples,
                            channels,
                            sample_rate,
                            |sample| (sample as f32 - 32_768.0) / 32_768.0,
                        )
                    },
                    error_callback,
                    None,
                )
            }
            sample_format => bail!("unsupported microphone sample format {sample_format:?}"),
        }
        .context("could not create microphone stream")?;
        stream.play().context("could not start microphone stream")?;

        Ok(Self {
            receiver,
            stream: Some(stream),
            sample_rate,
            dropped_frames,
        })
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn drain_into(&self, output: &mut super::SourceBuffer) {
        while let Ok(frame) = self.receiver.try_recv() {
            output.push(frame);
        }
    }

    pub fn stop(mut self, output: &mut super::SourceBuffer) -> CaptureStatistics {
        drop(self.stream.take());
        self.drain_into(output);
        CaptureStatistics {
            dropped_callback_frames: self.dropped_frames.load(Ordering::Relaxed),
            dropped_buffered_frames: output.dropped_frames(),
        }
    }
}

fn send_mono<T>(
    sender: &crossbeam_channel::Sender<AudioFrame>,
    dropped_frames: &AtomicU64,
    samples: &[T],
    channels: usize,
    sample_rate: u32,
    convert: impl Fn(T) -> f32,
) where
    T: Copy,
{
    if channels == 0 {
        return;
    }

    let captured_at = Instant::now();
    let samples = samples
        .chunks_exact(channels)
        .map(|frame| frame.iter().copied().map(&convert).sum::<f32>() / channels as f32)
        .collect();
    if sender
        .try_send(AudioFrame {
            source: SourceKind::Microphone,
            captured_at,
            sample_rate,
            samples,
        })
        .is_err()
    {
        dropped_frames.fetch_add(1, Ordering::Relaxed);
    }
}
