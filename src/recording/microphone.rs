use std::collections::VecDeque;

use anyhow::{bail, Context, Result};
use cpal::{
    traits::{DeviceTrait, HostTrait, StreamTrait},
    SampleFormat, Stream,
};
use crossbeam_channel::{bounded, Receiver};

pub struct MicrophoneCapture {
    receiver: Receiver<Vec<f32>>,
    _stream: Stream,
    sample_rate: u32,
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
        let error_callback = |error| eprintln!("microphone stream error: {error}");

        let stream = match config.sample_format() {
            SampleFormat::F32 => device.build_input_stream(
                &config.into(),
                move |samples: &[f32], _| send_mono(&sender, samples, channels, |sample| sample),
                error_callback,
                None,
            ),
            SampleFormat::I16 => device.build_input_stream(
                &config.into(),
                move |samples: &[i16], _| {
                    send_mono(&sender, samples, channels, |sample| {
                        sample as f32 / i16::MAX as f32
                    })
                },
                error_callback,
                None,
            ),
            SampleFormat::U16 => device.build_input_stream(
                &config.into(),
                move |samples: &[u16], _| {
                    send_mono(&sender, samples, channels, |sample| {
                        (sample as f32 - 32_768.0) / 32_768.0
                    })
                },
                error_callback,
                None,
            ),
            sample_format => bail!("unsupported microphone sample format {sample_format:?}"),
        }
        .context("could not create microphone stream")?;
        stream.play().context("could not start microphone stream")?;

        Ok(Self {
            receiver,
            _stream: stream,
            sample_rate,
        })
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn drain_into(&self, output: &mut VecDeque<f32>) {
        while let Ok(samples) = self.receiver.try_recv() {
            output.extend(samples);
        }
    }
}

fn send_mono<T>(
    sender: &crossbeam_channel::Sender<Vec<f32>>,
    samples: &[T],
    channels: usize,
    convert: impl Fn(T) -> f32,
) where
    T: Copy,
{
    if channels == 0 {
        return;
    }

    let mono = samples
        .chunks_exact(channels)
        .map(|frame| frame.iter().copied().map(&convert).sum::<f32>() / channels as f32)
        .collect();
    let _ = sender.try_send(mono);
}
