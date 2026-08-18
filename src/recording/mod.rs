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

use crate::{cli::RecordArgs, config::RecordingConfig};
use microphone::MicrophoneCapture;

const SAMPLE_RATE: u32 = 48_000;
const MIC_GAIN: f32 = 1.0;
const SYSTEM_GAIN: f32 = 0.8;

pub fn record(args: RecordArgs, config: Option<&RecordingConfig>) -> Result<()> {
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (args, config);
        bail!("system-audio recording is currently supported only on macOS")
    }

    #[cfg(target_os = "macos")]
    {
        record_macos(args, config)
    }
}

#[cfg(target_os = "macos")]
fn record_macos(args: RecordArgs, config: Option<&RecordingConfig>) -> Result<()> {
    let output_dir = output_dir(args.output.as_deref())?;
    let output_file = output_dir.join("audio.wav");
    let microphone_gain = config.map_or(MIC_GAIN, |config| config.microphone_gain);
    let system_gain = config.map_or(SYSTEM_GAIN, |config| config.system_gain);

    let stop = Arc::new(AtomicBool::new(false));
    let signal_stop = Arc::clone(&stop);
    ctrlc::set_handler(move || {
        signal_stop.store(true, Ordering::Release);
    })
    .context("could not install Ctrl-C handler")?;

    let microphone =
        MicrophoneCapture::start(config.and_then(|config| config.microphone_device.as_deref()))?;
    if microphone.sample_rate() != SAMPLE_RATE {
        bail!(
            "microphone uses {} Hz; only 48000 Hz is supported during the capture spike",
            microphone.sample_rate()
        )
    }

    let mut system = macos_system::SystemAudioCapture::start()?;
    if system.sample_rate() != SAMPLE_RATE {
        bail!(
            "system output uses {} Hz; only 48000 Hz is supported during the capture spike",
            system.sample_rate()
        )
    }

    // The microphone starts before the system tap is initialized. Discard that
    // pre-roll so the recording duration begins with both sources available.
    let mut microphone_samples = VecDeque::new();
    let mut system_samples = VecDeque::new();
    microphone.drain_into(&mut microphone_samples);
    system.drain_into(&mut system_samples);
    microphone_samples.clear();
    system_samples.clear();

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

    let started_at = Instant::now();
    let sample_limit = args
        .duration
        .map(|seconds| seconds.saturating_mul(SAMPLE_RATE as u64) as usize);
    let mut samples_written = 0;
    while !stop.load(Ordering::Acquire)
        && args
            .duration
            .is_none_or(|seconds| started_at.elapsed() < Duration::from_secs(seconds))
    {
        microphone.drain_into(&mut microphone_samples);
        system.drain_into(&mut system_samples);
        samples_written += write_available_samples(
            &mut writer,
            &mut microphone_samples,
            &mut system_samples,
            microphone_gain,
            system_gain,
            sample_limit.map(|limit| limit.saturating_sub(samples_written)),
        )?;
        thread::sleep(Duration::from_millis(10));
    }

    // The capture streams remain alive while their final callback frames are drained.
    thread::sleep(Duration::from_millis(20));
    microphone.drain_into(&mut microphone_samples);
    system.drain_into(&mut system_samples);
    write_available_samples(
        &mut writer,
        &mut microphone_samples,
        &mut system_samples,
        microphone_gain,
        system_gain,
        sample_limit.map(|limit| limit.saturating_sub(samples_written)),
    )?;
    writer.finalize().context("could not finalize WAV file")?;

    println!("Saved {}", output_file.display());
    Ok(())
}

fn write_available_samples<W: Write + Seek>(
    writer: &mut hound::WavWriter<W>,
    microphone: &mut VecDeque<f32>,
    system: &mut VecDeque<f32>,
    microphone_gain: f32,
    system_gain: f32,
    maximum_samples: Option<usize>,
) -> Result<usize> {
    let mut samples_written = 0;
    while (!microphone.is_empty() || !system.is_empty())
        && maximum_samples.is_none_or(|limit| samples_written < limit)
    {
        let mixed = microphone.pop_front().unwrap_or(0.0) * microphone_gain
            + system.pop_front().unwrap_or(0.0) * system_gain;
        let sample = (mixed.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16;
        writer
            .write_sample(sample)
            .context("could not write WAV sample")?;
        samples_written += 1;
    }
    Ok(samples_written)
}

fn output_dir(configured: Option<&Path>) -> Result<PathBuf> {
    let path = match configured {
        Some(path) => path.to_path_buf(),
        None => {
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .context("system time is before the Unix epoch")?
                .as_secs();
            PathBuf::from(format!("meetlite-{timestamp}"))
        }
    };

    fs::create_dir(&path)
        .with_context(|| format!("could not create output directory {}", path.display()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mix_clamps_to_the_i16_range() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: SAMPLE_RATE,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::new(file.reopen().unwrap(), spec).unwrap();
        let mut microphone = VecDeque::from([1.0]);
        let mut system = VecDeque::from([1.0]);

        write_available_samples(&mut writer, &mut microphone, &mut system, 1.0, 1.0, None).unwrap();
        writer.finalize().unwrap();

        let mut reader = hound::WavReader::open(file.path()).unwrap();
        assert_eq!(reader.samples::<i16>().next().unwrap().unwrap(), i16::MAX);
    }
}
