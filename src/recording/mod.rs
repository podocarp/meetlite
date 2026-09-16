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

use crate::{cli::CaptureArgs, config::RecordingConfig, live_control::LiveControl, output::Output};
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
const MAX_FRAME_TIMESTAMP_SKEW_SAMPLES: f64 = 32.0;
const MAX_INTERPOLATED_GAP_SAMPLES: usize = 32;
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RecordingSource {
    Microphone,
    System,
}

pub(super) type SourceKind = RecordingSource;

impl RecordingSource {
    pub(crate) fn speaker(self) -> &'static str {
        match self {
            Self::Microphone => "You",
            Self::System => "Remote",
        }
    }

    pub(crate) fn file_name(self) -> &'static str {
        match self {
            Self::Microphone => artifacts::MICROPHONE_FILE,
            Self::System => artifacts::SYSTEM_FILE,
        }
    }
}

struct AudioFrame {
    source: RecordingSource,
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
    source: RecordingSource,
    frames: VecDeque<AudioFrame>,
    next_frame_start: Option<Instant>,
    dropped_frames: u64,
}

impl SourceBuffer {
    fn new(source: RecordingSource) -> Self {
        Self {
            source,
            frames: VecDeque::new(),
            next_frame_start: None,
            dropped_frames: 0,
        }
    }

    fn push(&mut self, mut frame: AudioFrame) {
        if frame.source != self.source {
            self.dropped_frames += 1;
            return;
        }
        if frame.sample_rate == SAMPLE_RATE {
            if let Some(expected_start) = self.next_frame_start {
                let skew_samples =
                    signed_duration_seconds(frame.captured_at, expected_start) * SAMPLE_RATE as f64;
                if skew_samples.abs() <= MAX_FRAME_TIMESTAMP_SKEW_SAMPLES {
                    frame.captured_at = expected_start;
                }
            }
            self.next_frame_start = Some(frame.ends_at());
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
        let mut filled = vec![false; output.len()];
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
            for ((destination, occupied), sample) in output[output_start..output_start + count]
                .iter_mut()
                .zip(&mut filled[output_start..output_start + count])
                .zip(&frame.samples[input_start..input_start + count])
            {
                if !*occupied {
                    *destination = *sample;
                    *occupied = true;
                }
            }
        }

        Self::interpolate_tiny_gaps(output, &filled);
    }

    fn interpolate_tiny_gaps(output: &mut [f32], filled: &[bool]) {
        let mut index = 0;
        while index < filled.len() {
            if filled[index] {
                index += 1;
                continue;
            }
            let start = index;
            while index < filled.len() && !filled[index] {
                index += 1;
            }
            let gap = index - start;
            if gap > MAX_INTERPOLATED_GAP_SAMPLES || start == 0 || index >= filled.len() {
                continue;
            }
            let before = output[start - 1];
            let after = output[index];
            for offset in 0..gap {
                let ratio = (offset + 1) as f32 / (gap + 1) as f32;
                output[start + offset] = before + (after - before) * ratio;
            }
        }
    }
}

#[derive(Default)]
struct CaptureStatistics {
    dropped_callback_frames: u64,
    dropped_buffered_frames: u64,
}

struct SourceAligner {
    next_window: Instant,
    microphone: SourceBuffer,
    system: SourceBuffer,
    samples_written: usize,
    emitted_microphone_samples: Vec<i16>,
    emitted_system_samples: Vec<i16>,
}

impl SourceAligner {
    fn new(started_at: Instant) -> Self {
        Self {
            next_window: started_at,
            microphone: SourceBuffer::new(RecordingSource::Microphone),
            system: SourceBuffer::new(RecordingSource::System),
            samples_written: 0,
            emitted_microphone_samples: Vec::new(),
            emitted_system_samples: Vec::new(),
        }
    }

    fn write_ready<W: Write + Seek>(
        &mut self,
        mut microphone_writer: Option<&mut hound::WavWriter<W>>,
        mut system_writer: Option<&mut hound::WavWriter<W>>,
        now: Instant,
        sample_limit: Option<usize>,
    ) -> Result<()> {
        while now >= self.next_window + WINDOW_DURATION + WINDOW_DURATION
            && sample_limit.is_none_or(|limit| self.samples_written < limit)
        {
            self.write_window(
                microphone_writer.as_deref_mut(),
                system_writer.as_deref_mut(),
                sample_limit,
            )?;
        }
        Ok(())
    }

    fn flush<W: Write + Seek>(
        &mut self,
        mut microphone_writer: Option<&mut hound::WavWriter<W>>,
        mut system_writer: Option<&mut hound::WavWriter<W>>,
        sample_limit: Option<usize>,
    ) -> Result<()> {
        let last_frame_end = [self.microphone.last_end(), self.system.last_end()]
            .into_iter()
            .flatten()
            .max();
        while last_frame_end.is_some_and(|end| self.next_window < end)
            && sample_limit.is_none_or(|limit| self.samples_written < limit)
        {
            self.write_window(
                microphone_writer.as_deref_mut(),
                system_writer.as_deref_mut(),
                sample_limit,
            )?;
        }
        Ok(())
    }

    fn write_window<W: Write + Seek>(
        &mut self,
        mut microphone_writer: Option<&mut hound::WavWriter<W>>,
        mut system_writer: Option<&mut hound::WavWriter<W>>,
        sample_limit: Option<usize>,
    ) -> Result<()> {
        let mut microphone = vec![0.0; WINDOW_SAMPLES];
        let mut system = vec![0.0; WINDOW_SAMPLES];
        self.microphone
            .mix_window(self.next_window, &mut microphone);
        self.system.mix_window(self.next_window, &mut system);

        let remaining = sample_limit.map_or(WINDOW_SAMPLES, |limit| {
            limit
                .saturating_sub(self.samples_written)
                .min(WINDOW_SAMPLES)
        });
        for index in 0..remaining {
            if let Some(writer) = microphone_writer.as_mut() {
                let sample = to_pcm(microphone[index]);
                writer
                    .write_sample(sample)
                    .context("could not write microphone WAV sample")?;
                self.emitted_microphone_samples.push(sample);
            }
            if let Some(writer) = system_writer.as_mut() {
                let sample = to_pcm(system[index]);
                writer
                    .write_sample(sample)
                    .context("could not write system WAV sample")?;
                self.emitted_system_samples.push(sample);
            }
        }
        self.samples_written += remaining;
        self.next_window += WINDOW_DURATION;
        Ok(())
    }

    fn take_emitted_samples(&mut self, source: RecordingSource) -> Vec<i16> {
        match source {
            RecordingSource::Microphone => std::mem::take(&mut self.emitted_microphone_samples),
            RecordingSource::System => std::mem::take(&mut self.emitted_system_samples),
        }
    }
}

fn to_pcm(sample: f32) -> i16 {
    (sample.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16
}

pub fn record(args: CaptureArgs, config: Option<&RecordingConfig>, output: Output) -> Result<()> {
    let control = LiveControl::install()?;
    let recording = record_with_samples(
        args,
        config,
        Some(control),
        output,
        |recording| {
            if output.is_json() {
                output.event(&serde_json::json!({
                    "type": "lifecycle",
                    "phase": "recording_started",
                    "output_dir": recording.output_dir.display().to_string(),
                }))?;
            }
            Ok(())
        },
        |_, _| {},
    )?;
    if output.is_json() {
        output.event(&serde_json::json!({
            "type": "lifecycle",
            "phase": "recording_stopped",
            "output_dir": recording.output_dir.display().to_string(),
        }))?;
    } else {
        output.status(
            "Saved recording",
            &recording.output_dir.display().to_string(),
        );
    }
    Ok(())
}

pub fn record_with_samples(
    args: CaptureArgs,
    config: Option<&RecordingConfig>,
    control: Option<LiveControl>,
    output: Output,
    on_started: impl FnOnce(&RecordingOutput) -> Result<()>,
    on_samples: impl FnMut(RecordingSource, &[i16]),
) -> Result<RecordingOutput> {
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (args, config, output, on_started, on_samples);
        anyhow::bail!("recording is currently supported only on macOS and Linux")
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        let plan = RecordingPlan::from_args(args, config)?;
        let factory = PlatformCaptureAdapterFactory;
        RecordingSession::new(
            plan,
            &factory,
            CallbackSink::new(on_started, on_samples),
            control,
            output,
        )
        .run()
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn signed_duration_seconds(left: Instant, right: Instant) -> f64 {
    if left >= right {
        left.duration_since(right).as_secs_f64()
    } else {
        -right.duration_since(left).as_secs_f64()
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn drain_sources(
    microphone: &mut Option<BoxedCaptureAdapter>,
    system: &mut Option<BoxedCaptureAdapter>,
    mixer: &mut SourceAligner,
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

    fn frame(source: RecordingSource, start: Instant, samples: Vec<f32>) -> AudioFrame {
        AudioFrame {
            source,
            captured_at: start,
            sample_rate: SAMPLE_RATE,
            samples,
        }
    }

    #[test]
    fn source_aligner_aligns_sources_and_zero_fills_missing_windows() {
        let start = Instant::now();
        let mut mixer = SourceAligner::new(start);
        mixer.microphone.push(frame(
            RecordingSource::Microphone,
            start,
            vec![0.25; WINDOW_SAMPLES],
        ));
        mixer.system.push(frame(
            RecordingSource::System,
            start + Duration::from_millis(20),
            vec![0.5; WINDOW_SAMPLES],
        ));

        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: SAMPLE_RATE,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let microphone_file = tempfile::NamedTempFile::new().unwrap();
        let system_file = tempfile::NamedTempFile::new().unwrap();
        let mut microphone_writer =
            hound::WavWriter::new(microphone_file.reopen().unwrap(), spec).unwrap();
        let mut system_writer = hound::WavWriter::new(system_file.reopen().unwrap(), spec).unwrap();
        mixer
            .flush(Some(&mut microphone_writer), Some(&mut system_writer), None)
            .unwrap();
        let emitted_microphone = mixer.take_emitted_samples(RecordingSource::Microphone);
        let emitted_system = mixer.take_emitted_samples(RecordingSource::System);
        microphone_writer.finalize().unwrap();
        system_writer.finalize().unwrap();
        let microphone_samples: Vec<i16> = hound::WavReader::open(microphone_file.path())
            .unwrap()
            .samples::<i16>()
            .map(Result::unwrap)
            .collect();
        let system_samples: Vec<i16> = hound::WavReader::open(system_file.path())
            .unwrap()
            .samples::<i16>()
            .map(Result::unwrap)
            .collect();
        assert_eq!(emitted_microphone, microphone_samples);
        assert_eq!(emitted_system, system_samples);
        assert_eq!(microphone_samples.len(), system_samples.len());
        assert_eq!(
            microphone_samples[0],
            (0.25 * i16::MAX as f32).round() as i16
        );
        assert_eq!(microphone_samples[WINDOW_SAMPLES], 0);
        assert_eq!(system_samples[0], 0);
        assert_eq!(
            system_samples[WINDOW_SAMPLES],
            (0.5 * i16::MAX as f32).round() as i16
        );
    }

    #[test]
    fn forced_output_directory_removes_only_meetlite_artifacts() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("audio.wav"), "legacy audio").unwrap();
        fs::write(directory.path().join("notes.txt"), "keep me").unwrap();
        for name in [
            "microphone.wav",
            "system.wav",
            "metadata.json.tmp",
            "transcript.json.tmp",
            "summary.md.tmp",
        ] {
            fs::write(directory.path().join(name), "old artifact").unwrap();
        }
        fs::create_dir(directory.path().join("chunks")).unwrap();
        fs::write(directory.path().join("chunks/old.wav"), "old chunk").unwrap();

        assert!(artifacts::output_dir(Some(directory.path()), false).is_err());
        artifacts::output_dir(Some(directory.path()), true).unwrap();

        assert_eq!(
            fs::read_to_string(directory.path().join("audio.wav")).unwrap(),
            "legacy audio"
        );
        assert!(!directory.path().join("chunks").exists());
        for name in [
            "microphone.wav",
            "system.wav",
            "metadata.json.tmp",
            "transcript.json.tmp",
            "summary.md.tmp",
        ] {
            assert!(!directory.path().join(name).exists());
        }
        assert_eq!(
            fs::read_to_string(directory.path().join("notes.txt")).unwrap(),
            "keep me"
        );
    }

    #[test]
    fn source_buffer_does_not_sum_overlapping_frames_from_the_same_source() {
        let start = Instant::now();
        let mut buffer = SourceBuffer::new(RecordingSource::System);
        buffer.push(frame(RecordingSource::System, start, vec![0.25; 2]));
        buffer.push(frame(
            RecordingSource::System,
            start + Duration::from_secs_f64(1.0 / SAMPLE_RATE as f64),
            vec![0.5; 2],
        ));

        let mut output = vec![0.0; 3];
        buffer.mix_window(start, &mut output);

        assert_eq!(output, vec![0.25, 0.25, 0.5]);
    }

    #[test]
    fn source_buffer_interpolates_tiny_timestamp_gaps() {
        let mut output = vec![0.25, 0.25, 0.0, 0.75];
        let filled = vec![true, true, false, true];

        SourceBuffer::interpolate_tiny_gaps(&mut output, &filled);

        assert_eq!(output, vec![0.25, 0.25, 0.5, 0.75]);
    }

    #[test]
    fn source_aligner_writes_a_partial_final_window_at_the_requested_duration() {
        let start = Instant::now();
        let mut mixer = SourceAligner::new(start);
        mixer.microphone.push(frame(
            RecordingSource::Microphone,
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
        mixer.flush(Some(&mut writer), None, Some(100)).unwrap();
        writer.finalize().unwrap();

        assert_eq!(hound::WavReader::open(file.path()).unwrap().len(), 100);
    }

    #[test]
    fn source_buffer_discards_oldest_frames_when_full() {
        let start = Instant::now();
        let mut buffer = SourceBuffer::new(RecordingSource::Microphone);
        for _ in 0..=MAX_BUFFERED_FRAMES {
            buffer.push(frame(RecordingSource::Microphone, start, vec![0.0]));
        }
        assert_eq!(buffer.frames.len(), MAX_BUFFERED_FRAMES);
        assert_eq!(buffer.dropped_frames(), 1);
    }
}
