#[cfg(target_os = "macos")]
mod macos_capture_agent;
#[cfg(target_os = "macos")]
mod macos_system;
mod microphone;

use std::{
    collections::VecDeque,
    fs,
    io::{Seek, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{bail, Context, Result};
use serde::Serialize;

use crate::{cli::RecordArgs, config::RecordingConfig};
#[cfg(target_os = "macos")]
pub(crate) use macos_capture_agent::run_capture_agent;
#[cfg(target_os = "macos")]
use macos_capture_agent::AgentSystemAudioCapture;
use microphone::MicrophoneCapture;

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

#[derive(Clone)]
pub struct RecordingOutput {
    pub output_dir: PathBuf,
    pub audio_file: PathBuf,
}

pub fn record_with_samples(
    args: RecordArgs,
    config: Option<&RecordingConfig>,
    on_started: impl FnOnce(&RecordingOutput),
    on_samples: impl FnMut(&[i16]),
) -> Result<RecordingOutput> {
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (args, config, on_started, on_samples);
        bail!("system-audio recording is currently supported only on macOS")
    }

    #[cfg(target_os = "macos")]
    {
        record_macos(args, config, on_started, on_samples)
    }
}

#[cfg(target_os = "macos")]
fn record_macos(
    args: RecordArgs,
    config: Option<&RecordingConfig>,
    on_started: impl FnOnce(&RecordingOutput),
    mut on_samples: impl FnMut(&[i16]),
) -> Result<RecordingOutput> {
    if args.no_microphone && args.no_system_audio {
        bail!("at least one audio source must be enabled")
    }

    let output_dir = output_dir(args.output.as_deref())?;
    let output_file = output_dir.join("audio.wav");
    on_started(&RecordingOutput {
        output_dir: output_dir.clone(),
        audio_file: output_file.clone(),
    });
    let microphone_gain = args
        .microphone_gain
        .unwrap_or_else(|| config.map_or(MIC_GAIN, |config| config.microphone_gain));
    let system_gain = args
        .system_gain
        .unwrap_or_else(|| config.map_or(SYSTEM_GAIN, |config| config.system_gain));
    if !microphone_gain.is_finite()
        || microphone_gain < 0.0
        || !system_gain.is_finite()
        || system_gain < 0.0
    {
        bail!("recording gains must be finite, non-negative numbers")
    }

    let stop = Arc::new(AtomicBool::new(false));
    let signal_stop = Arc::clone(&stop);
    ctrlc::set_handler(move || signal_stop.store(true, Ordering::Release))
        .context("could not install Ctrl-C handler")?;

    let mut microphone = (!args.no_microphone)
        .then(|| {
            MicrophoneCapture::start(config.and_then(|config| config.microphone_device.as_deref()))
        })
        .transpose()?;
    if microphone
        .as_ref()
        .is_some_and(|capture| capture.sample_rate() != SAMPLE_RATE)
    {
        bail!("microphone does not deliver 48000 Hz; resampling is not implemented yet")
    }
    let mut system = (!args.no_system_audio)
        .then(AgentSystemAudioCapture::start)
        .transpose()?;
    if system
        .as_ref()
        .is_some_and(|capture| capture.sample_rate() != SAMPLE_RATE)
    {
        bail!("system output does not deliver 48000 Hz; resampling is not implemented yet")
    }

    let started_at = Instant::now();
    let started_at_unix_ms = unix_time_ms();
    let mut mixer = Mixer::new(started_at, microphone_gain, system_gain);
    let specification = hound::WavSpec {
        channels: 1,
        sample_rate: SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(&output_file, specification)
        .with_context(|| format!("could not create {}", output_file.display()))?;

    println!("Recording to {}", output_file.display());
    println!("Press Ctrl-C to stop.");
    let sample_limit = args
        .duration
        .map(|seconds| seconds.saturating_mul(SAMPLE_RATE as u64) as usize);
    while !stop.load(Ordering::Acquire)
        && args
            .duration
            .is_none_or(|seconds| started_at.elapsed() < Duration::from_secs(seconds))
    {
        drain_sources(&mut microphone, &mut system, &mut mixer);
        mixer.write_ready(&mut writer, Instant::now(), sample_limit)?;
        let samples = mixer.take_emitted_samples();
        on_samples(&samples);
        thread::sleep(Duration::from_millis(5));
    }

    // Dropping each stream is the producer acknowledgement: callbacks can no
    // longer enqueue frames before the final source queues are drained.
    let microphone_stats = microphone
        .take()
        .map(|capture| capture.stop(&mut mixer.microphone));
    let system_stats = system.take().map(|capture| capture.stop(&mut mixer.system));
    mixer.flush(&mut writer, sample_limit)?;
    let samples = mixer.take_emitted_samples();
    on_samples(&samples);
    writer.finalize().context("could not finalize WAV file")?;

    write_metadata(
        &output_dir,
        Metadata {
            started_at_unix_ms,
            ended_at_unix_ms: unix_time_ms(),
            audio_file: "audio.wav",
            sample_rate: SAMPLE_RATE,
            channels: 1,
            bits_per_sample: 16,
            samples_written: mixer.samples_written,
            microphone: SourceMetadata::from_capture(
                !args.no_microphone,
                microphone_gain,
                microphone_stats,
            ),
            system: SourceMetadata::from_capture(!args.no_system_audio, system_gain, system_stats),
        },
    )?;
    println!("Saved {}", output_file.display());
    Ok(RecordingOutput {
        output_dir,
        audio_file: output_file,
    })
}

#[cfg(target_os = "macos")]
fn drain_sources(
    microphone: &mut Option<MicrophoneCapture>,
    system: &mut Option<AgentSystemAudioCapture>,
    mixer: &mut Mixer,
) {
    if let Some(capture) = microphone {
        capture.drain_into(&mut mixer.microphone);
    }
    if let Some(capture) = system {
        capture.drain_into(&mut mixer.system);
    }
}

#[derive(Serialize)]
struct Metadata<'a> {
    started_at_unix_ms: u128,
    ended_at_unix_ms: u128,
    audio_file: &'a str,
    sample_rate: u32,
    channels: u8,
    bits_per_sample: u8,
    samples_written: usize,
    microphone: SourceMetadata,
    system: SourceMetadata,
}

#[derive(Serialize)]
struct SourceMetadata {
    enabled: bool,
    gain: f32,
    dropped_callback_frames: u64,
    dropped_buffered_frames: u64,
}

impl SourceMetadata {
    fn from_capture(enabled: bool, gain: f32, statistics: Option<CaptureStatistics>) -> Self {
        let statistics = statistics.unwrap_or_default();
        Self {
            enabled,
            gain,
            dropped_callback_frames: statistics.dropped_callback_frames,
            dropped_buffered_frames: statistics.dropped_buffered_frames,
        }
    }
}

fn write_metadata(output_dir: &Path, metadata: Metadata<'_>) -> Result<()> {
    let path = output_dir.join("metadata.json");
    let temporary_path = output_dir.join("metadata.json.tmp");
    let contents = serde_json::to_vec_pretty(&metadata)?;
    fs::write(&temporary_path, contents)
        .with_context(|| format!("could not write {}", temporary_path.display()))?;
    fs::rename(&temporary_path, &path)
        .with_context(|| format!("could not atomically write {}", path.display()))
}

fn unix_time_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time is after the Unix epoch")
        .as_millis()
}

fn output_dir(configured: Option<&Path>) -> Result<PathBuf> {
    let path = match configured {
        Some(path) => path.to_path_buf(),
        None => PathBuf::from(format!("meetlite-{}", unix_time_ms())),
    };
    fs::create_dir(&path)
        .with_context(|| format!("could not create output directory {}", path.display()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
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
