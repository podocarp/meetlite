mod adapter;
mod artifacts;
#[cfg(target_os = "linux")]
mod linux_system;
#[cfg(target_os = "macos")]
mod macos_capture_agent;
#[cfg(target_os = "macos")]
mod macos_system;
mod microphone;
mod plan;
#[cfg(target_os = "linux")]
mod pulse_system;
mod session;

use std::{
    collections::VecDeque,
    io::{Seek, Write},
    time::{Duration, Instant},
};

use anyhow::{Context, Result};

use crate::{cli::RecordArgs, config::RecordingConfig};
use adapter::BoxedCaptureAdapter;
use adapter::PlatformCaptureAdapterFactory;
pub use artifacts::RecordingOutput;
#[cfg(target_os = "macos")]
pub(crate) use macos_capture_agent::run_capture_agent;
use plan::RecordingPlan;
use session::{CallbackSink, RecordingSession};

const SAMPLE_RATE: u32 = 48_000;
const WINDOW_SAMPLES: usize = 960;
const WINDOW_DURATION: Duration = Duration::from_millis(20);
const MAX_BUFFERED_FRAMES: usize = 256;
const MIC_GAIN: f32 = 1.0;
const SYSTEM_GAIN: f32 = 0.8;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SourceKind {
    Microphone,
    System,
}

struct AudioFrame {
    source: SourceKind,
    captured_at: Instant,
    sample_rate: u32,
    samples: Vec<f32>,
}

impl AudioFrame {
    fn ends_at(&self) -> Instant {
        self.captured_at
            + Duration::from_secs_f64(self.samples.len() as f64 / self.sample_rate as f64)
    }
}

struct SourceBuffer {
    source: SourceKind,
    frames: VecDeque<AudioFrame>,
    dropped_frames: u64,
}

impl SourceBuffer {
    fn new(source: SourceKind) -> Self {
        Self {
            source,
            frames: VecDeque::new(),
            dropped_frames: 0,
        }
    }

    fn push(&mut self, frame: AudioFrame) {
        if frame.source != self.source {
            self.dropped_frames += 1;
            return;
        }
        if self.frames.len() == MAX_BUFFERED_FRAMES {
            self.frames.pop_front();
            self.dropped_frames += 1;
        }
        self.frames.push_back(frame);
    }

    fn dropped_frames(&self) -> u64 {
        self.dropped_frames
    }

    #[cfg(target_os = "macos")]
    fn pop_frame(&mut self) -> Option<AudioFrame> {
        self.frames.pop_front()
    }

    fn last_end(&self) -> Option<Instant> {
        self.frames.back().map(AudioFrame::ends_at)
    }

    fn mix_window(&mut self, start: Instant, output: &mut [f32]) {
        while self
            .frames
            .front()
            .is_some_and(|frame| frame.ends_at() <= start)
        {
            self.frames.pop_front();
        }

        let end = start + WINDOW_DURATION;
        for frame in &self.frames {
            if frame.captured_at >= end {
                break;
            }
            if frame.sample_rate != SAMPLE_RATE {
                continue;
            }

            let offset = if frame.captured_at >= start {
                frame.captured_at.duration_since(start).as_secs_f64()
            } else {
                -start.duration_since(frame.captured_at).as_secs_f64()
            };
            let output_start = (offset * SAMPLE_RATE as f64).round().max(0.0) as usize;
            let input_start = (-offset * SAMPLE_RATE as f64).round().max(0.0) as usize;
            if output_start >= output.len() || input_start >= frame.samples.len() {
                continue;
            }
            let count = (output.len() - output_start).min(frame.samples.len() - input_start);
            for (destination, sample) in output[output_start..output_start + count]
                .iter_mut()
                .zip(&frame.samples[input_start..input_start + count])
            {
                *destination += sample;
            }
        }
    }
}

#[derive(Default)]
struct CaptureStatistics {
    dropped_callback_frames: u64,
    dropped_buffered_frames: u64,
}

struct Mixer {
    next_window: Instant,
    microphone: SourceBuffer,
    system: SourceBuffer,
    microphone_gain: f32,
    system_gain: f32,
    samples_written: usize,
    emitted_samples: Vec<i16>,
}

impl Mixer {
    fn new(started_at: Instant, microphone_gain: f32, system_gain: f32) -> Self {
        Self {
            next_window: started_at,
            microphone: SourceBuffer::new(SourceKind::Microphone),
            system: SourceBuffer::new(SourceKind::System),
            microphone_gain,
            system_gain,
            samples_written: 0,
            emitted_samples: Vec::new(),
        }
    }

    fn write_ready<W: Write + Seek>(
        &mut self,
        writer: &mut hound::WavWriter<W>,
        now: Instant,
        sample_limit: Option<usize>,
    ) -> Result<()> {
        // Wait one additional window so callback scheduling jitter does not turn
        // an otherwise available source frame into artificial silence.
        while now >= self.next_window + WINDOW_DURATION + WINDOW_DURATION
            && sample_limit.is_none_or(|limit| self.samples_written < limit)
        {
            self.write_window(writer, sample_limit)?;
        }
        Ok(())
    }

    fn flush<W: Write + Seek>(
        &mut self,
        writer: &mut hound::WavWriter<W>,
        sample_limit: Option<usize>,
    ) -> Result<()> {
        let last_frame_end = [self.microphone.last_end(), self.system.last_end()]
            .into_iter()
            .flatten()
            .max();
        while last_frame_end.is_some_and(|end| self.next_window < end)
            && sample_limit.is_none_or(|limit| self.samples_written < limit)
        {
            self.write_window(writer, sample_limit)?;
        }
        Ok(())
    }

    fn write_window<W: Write + Seek>(
        &mut self,
        writer: &mut hound::WavWriter<W>,
        sample_limit: Option<usize>,
    ) -> Result<()> {
        let mut microphone = vec![0.0; WINDOW_SAMPLES];
        let mut system = vec![0.0; WINDOW_SAMPLES];
        self.microphone
            .mix_window(self.next_window, &mut microphone);
        self.system.mix_window(self.next_window, &mut system);

        let mut mixed: Vec<f32> = microphone
            .iter()
            .zip(&system)
            .map(|(microphone, system)| {
                microphone * self.microphone_gain + system * self.system_gain
            })
            .collect();
        if let Some(peak) = mixed.iter().map(|sample| sample.abs()).reduce(f32::max) {
            if peak > 1.0 {
                for sample in &mut mixed {
                    *sample /= peak;
                }
            }
        }

        let remaining = sample_limit.map_or(WINDOW_SAMPLES, |limit| {
            limit
                .saturating_sub(self.samples_written)
                .min(WINDOW_SAMPLES)
        });
        for sample in mixed.into_iter().take(remaining) {
            let sample = (sample.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16;
            writer
                .write_sample(sample)
                .context("could not write WAV sample")?;
            self.emitted_samples.push(sample);
        }
        self.samples_written += remaining;
        self.next_window += WINDOW_DURATION;
        Ok(())
    }

    fn take_emitted_samples(&mut self) -> Vec<i16> {
        std::mem::take(&mut self.emitted_samples)
    }
}

pub fn record(args: RecordArgs, config: Option<&RecordingConfig>) -> Result<()> {
    record_with_samples(args, config, |_| {}, |_| {}).map(|_| ())
}

pub fn record_with_samples(
    args: RecordArgs,
    config: Option<&RecordingConfig>,
    on_started: impl FnOnce(&RecordingOutput),
    on_samples: impl FnMut(&[i16]),
) -> Result<RecordingOutput> {
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (args, config, on_started, on_samples);
        anyhow::bail!("recording is currently supported only on macOS and Linux")
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        let plan = RecordingPlan::from_args(args, config)?;
        let factory = PlatformCaptureAdapterFactory;
        RecordingSession::new(plan, &factory, CallbackSink::new(on_started, on_samples)).run()
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn drain_sources(
    microphone: &mut Option<BoxedCaptureAdapter>,
    system: &mut Option<BoxedCaptureAdapter>,
    mixer: &mut Mixer,
) {
    if let Some(capture) = microphone {
        capture.drain_into(&mut mixer.microphone);
    }
    if let Some(capture) = system {
        capture.drain_into(&mut mixer.system);
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn frame(source: SourceKind, start: Instant, samples: Vec<f32>) -> AudioFrame {
        AudioFrame {
            source,
            captured_at: start,
            sample_rate: SAMPLE_RATE,
            samples,
        }
    }

    #[test]
    fn mixer_aligns_sources_and_zero_fills_missing_windows() {
        let start = Instant::now();
        let mut mixer = Mixer::new(start, 1.0, 1.0);
        mixer.microphone.push(frame(
            SourceKind::Microphone,
            start,
            vec![0.25; WINDOW_SAMPLES],
        ));
        mixer.system.push(frame(
            SourceKind::System,
            start + Duration::from_millis(20),
            vec![0.5; WINDOW_SAMPLES],
        ));

        let file = tempfile::NamedTempFile::new().unwrap();
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: SAMPLE_RATE,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::new(file.reopen().unwrap(), spec).unwrap();
        mixer.flush(&mut writer, None).unwrap();
        writer.finalize().unwrap();

        let samples: Vec<i16> = hound::WavReader::open(file.path())
            .unwrap()
            .samples::<i16>()
            .map(Result::unwrap)
            .collect();
        assert_eq!(samples.len(), WINDOW_SAMPLES * 2);
        assert_eq!(samples[0], (0.25 * i16::MAX as f32).round() as i16);
        assert_eq!(
            samples[WINDOW_SAMPLES],
            (0.5 * i16::MAX as f32).round() as i16
        );
    }

    #[test]
    fn forced_output_directory_removes_only_meetlite_artifacts() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("audio.wav"), "old audio").unwrap();
        fs::write(directory.path().join("notes.txt"), "keep me").unwrap();
        fs::create_dir(directory.path().join("chunks")).unwrap();
        fs::write(directory.path().join("chunks/old.wav"), "old chunk").unwrap();

        assert!(artifacts::output_dir(Some(directory.path()), false).is_err());
        artifacts::output_dir(Some(directory.path()), true).unwrap();

        assert!(!directory.path().join("audio.wav").exists());
        assert!(!directory.path().join("chunks").exists());
        assert_eq!(
            fs::read_to_string(directory.path().join("notes.txt")).unwrap(),
            "keep me"
        );
    }

    #[test]
    fn mixer_scales_an_overloaded_window_instead_of_clipping_each_sample() {
        let start = Instant::now();
        let mut mixer = Mixer::new(start, 1.0, 1.0);
        mixer.microphone.push(frame(
            SourceKind::Microphone,
            start,
            vec![1.0; WINDOW_SAMPLES],
        ));
        mixer
            .system
            .push(frame(SourceKind::System, start, vec![0.5; WINDOW_SAMPLES]));

        let file = tempfile::NamedTempFile::new().unwrap();
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: SAMPLE_RATE,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::new(file.reopen().unwrap(), spec).unwrap();
        mixer.flush(&mut writer, None).unwrap();
        writer.finalize().unwrap();

        let sample = hound::WavReader::open(file.path())
            .unwrap()
            .samples::<i16>()
            .next()
            .unwrap()
            .unwrap();
        assert_eq!(sample, i16::MAX);
    }

    #[test]
    fn mixer_writes_a_partial_final_window_at_the_requested_duration() {
        let start = Instant::now();
        let mut mixer = Mixer::new(start, 1.0, 0.0);
        mixer.microphone.push(frame(
            SourceKind::Microphone,
            start,
            vec![0.25; WINDOW_SAMPLES],
        ));

        let file = tempfile::NamedTempFile::new().unwrap();
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: SAMPLE_RATE,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::new(file.reopen().unwrap(), spec).unwrap();
        mixer.flush(&mut writer, Some(100)).unwrap();
        writer.finalize().unwrap();

        assert_eq!(hound::WavReader::open(file.path()).unwrap().len(), 100);
    }

    #[test]
    fn source_buffer_discards_oldest_frames_when_full() {
        let start = Instant::now();
        let mut buffer = SourceBuffer::new(SourceKind::Microphone);
        for _ in 0..=MAX_BUFFERED_FRAMES {
            buffer.push(frame(SourceKind::Microphone, start, vec![0.0]));
        }
        assert_eq!(buffer.frames.len(), MAX_BUFFERED_FRAMES);
        assert_eq!(buffer.dropped_frames(), 1);
    }
}
