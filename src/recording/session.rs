use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{bail, Context, Result};

use super::{
    adapter::{BoxedCaptureAdapter, CaptureAdapterFactory},
    artifacts::{
        audio_file_name, unix_time_ms, RecordingArtifacts, RecordingMetadata, RecordingOutput,
        SourceMetadata,
    },
    drain_sources,
    plan::RecordingPlan,
    Mixer, SAMPLE_RATE,
};

pub(super) trait RecordingSink {
    fn recording_started(&mut self, output: &RecordingOutput);
    fn samples_written(&mut self, samples: &[i16]);
}

pub(super) struct CallbackSink<Started, Samples> {
    on_started: Option<Started>,
    on_samples: Samples,
}

impl<Started, Samples> CallbackSink<Started, Samples> {
    pub(super) fn new(on_started: Started, on_samples: Samples) -> Self {
        Self {
            on_started: Some(on_started),
            on_samples,
        }
    }
}

impl<Started, Samples> RecordingSink for CallbackSink<Started, Samples>
where
    Started: FnOnce(&RecordingOutput),
    Samples: FnMut(&[i16]),
{
    fn recording_started(&mut self, output: &RecordingOutput) {
        if let Some(on_started) = self.on_started.take() {
            on_started(output);
        }
    }

    fn samples_written(&mut self, samples: &[i16]) {
        (self.on_samples)(samples);
    }
}

pub(super) struct RecordingSession<'a, F, S> {
    plan: RecordingPlan,
    factory: &'a F,
    sink: S,
}

impl<'a, F, S> RecordingSession<'a, F, S>
where
    F: CaptureAdapterFactory,
    S: RecordingSink,
{
    pub(super) fn new(plan: RecordingPlan, factory: &'a F, sink: S) -> Self {
        Self {
            plan,
            factory,
            sink,
        }
    }

    pub(super) fn run(mut self) -> Result<RecordingOutput> {
        let artifacts = RecordingArtifacts::prepare(self.plan.output.as_deref(), self.plan.force)?;
        let output = artifacts.output();
        self.sink.recording_started(&output);

        let stop = Arc::new(AtomicBool::new(false));
        let signal_stop = Arc::clone(&stop);
        ctrlc::set_handler(move || signal_stop.store(true, Ordering::Release))
            .context("could not install Ctrl-C handler")?;

        let mut microphone = self.start_microphone()?;
        let mut system = self.start_system()?;

        let started_at = Instant::now();
        let started_at_unix_ms = unix_time_ms();
        let mut mixer = Mixer::new(started_at, self.plan.microphone_gain, self.plan.system_gain);
        let specification = hound::WavSpec {
            channels: 1,
            sample_rate: SAMPLE_RATE,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(artifacts.audio_file(), specification)
            .with_context(|| format!("could not create {}", artifacts.audio_file().display()))?;

        println!("Recording to {}", artifacts.audio_file().display());
        println!("Press Ctrl-C to stop.");
        let sample_limit = self.plan.sample_limit();
        while !stop.load(Ordering::Acquire)
            && self
                .plan
                .duration_seconds
                .is_none_or(|seconds| started_at.elapsed() < Duration::from_secs(seconds))
        {
            drain_sources(&mut microphone, &mut system, &mut mixer);
            mixer.write_ready(&mut writer, Instant::now(), sample_limit)?;
            let samples = mixer.take_emitted_samples();
            self.sink.samples_written(&samples);
            thread::sleep(Duration::from_millis(5));
        }

        let microphone_stats = microphone
            .take()
            .map(|capture| capture.stop(&mut mixer.microphone));
        let system_stats = system.take().map(|capture| capture.stop(&mut mixer.system));
        mixer.flush(&mut writer, sample_limit)?;
        let samples = mixer.take_emitted_samples();
        self.sink.samples_written(&samples);
        writer.finalize().context("could not finalize WAV file")?;

        artifacts.write_metadata(RecordingMetadata {
            started_at_unix_ms,
            ended_at_unix_ms: unix_time_ms(),
            audio_file: audio_file_name(),
            sample_rate: SAMPLE_RATE,
            channels: 1,
            bits_per_sample: 16,
            samples_written: mixer.samples_written,
            microphone: SourceMetadata::from_capture(
                self.plan.microphone_enabled(),
                self.plan.microphone_gain,
                microphone_stats,
            ),
            system: SourceMetadata::from_capture(
                self.plan.system_enabled(),
                self.plan.system_gain,
                system_stats,
            ),
        })?;
        println!("Saved {}", artifacts.audio_file().display());
        Ok(output)
    }

    fn start_microphone(&self) -> Result<Option<BoxedCaptureAdapter>> {
        let Some(source) = &self.plan.microphone else {
            return Ok(None);
        };
        let capture = self
            .factory
            .start_microphone(source.device_name.as_deref())?;
        if capture.sample_rate() != SAMPLE_RATE {
            bail!("microphone does not deliver 48000 Hz; resampling is not implemented yet")
        }
        Ok(Some(capture))
    }

    fn start_system(&self) -> Result<Option<BoxedCaptureAdapter>> {
        let Some(source) = &self.plan.system else {
            return Ok(None);
        };
        let capture = self
            .factory
            .start_system_audio(source.device_name.as_deref())?;
        if capture.sample_rate() != SAMPLE_RATE {
            bail!("system output does not deliver 48000 Hz; resampling is not implemented yet")
        }
        Ok(Some(capture))
    }
}
