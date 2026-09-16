use std::{
    thread,
    time::{Duration, Instant},
};

use anyhow::{bail, Context, Result};

use crate::{live_control::LiveControl, output::Output};

use super::{
    adapter::{BoxedCaptureAdapter, CaptureAdapterFactory},
    artifacts::{
        unix_time_ms, RecordingArtifacts, RecordingMetadata, RecordingOutput, SourceMetadata,
        MICROPHONE_FILE, SYSTEM_FILE,
    },
    drain_sources,
    plan::RecordingPlan,
    RecordingSource, SourceAligner, SAMPLE_RATE,
};

pub(super) trait RecordingSink {
    fn recording_started(&mut self, output: &RecordingOutput) -> Result<()>;
    fn samples_written(&mut self, source: RecordingSource, samples: &[i16]);
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
    Started: FnOnce(&RecordingOutput) -> Result<()>,
    Samples: FnMut(RecordingSource, &[i16]),
{
    fn recording_started(&mut self, output: &RecordingOutput) -> Result<()> {
        if let Some(on_started) = self.on_started.take() {
            on_started(output)?;
        }
        Ok(())
    }

    fn samples_written(&mut self, source: RecordingSource, samples: &[i16]) {
        (self.on_samples)(source, samples);
    }
}

pub(super) struct RecordingSession<'a, F, S> {
    plan: RecordingPlan,
    factory: &'a F,
    sink: S,
    control: Option<LiveControl>,
    output: Output,
}

impl<'a, F, S> RecordingSession<'a, F, S>
where
    F: CaptureAdapterFactory,
    S: RecordingSink,
{
    pub(super) fn new(
        plan: RecordingPlan,
        factory: &'a F,
        sink: S,
        control: Option<LiveControl>,
        output: Output,
    ) -> Self {
        Self {
            plan,
            factory,
            sink,
            control,
            output,
        }
    }

    pub(super) fn run(mut self) -> Result<RecordingOutput> {
        let artifacts = RecordingArtifacts::prepare(
            self.plan.output.as_deref(),
            self.plan.force,
            self.plan.microphone_enabled(),
            self.plan.system_enabled(),
        )?;
        let output = artifacts.output();

        let mut microphone = self.start_microphone()?;
        let mut system = self.start_system()?;

        let started_at = Instant::now();
        let started_at_unix_ms = unix_time_ms();
        let mut aligner = SourceAligner::new(started_at);
        let specification = hound::WavSpec {
            channels: 1,
            sample_rate: SAMPLE_RATE,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut microphone_writer = artifacts
            .microphone_file()
            .map(|path| hound::WavWriter::create(path, specification))
            .transpose()
            .with_context(|| "could not create microphone WAV file")?;
        let mut system_writer = artifacts
            .system_file()
            .map(|path| hound::WavWriter::create(path, specification))
            .transpose()
            .with_context(|| "could not create system WAV file")?;

        self.sink.recording_started(&output)?;

        let terminal = self.output;
        terminal.status("Recording", &format!("to {}", output.output_dir.display()));
        terminal.instruction("Press Ctrl-C to stop recording. Press Ctrl-D to quit immediately.");
        terminal.blank_line();
        let sample_limit = self.plan.sample_limit();
        while !self
            .control
            .as_ref()
            .is_some_and(LiveControl::recording_stopped)
            && self
                .plan
                .duration_seconds
                .is_none_or(|seconds| started_at.elapsed() < Duration::from_secs(seconds))
        {
            drain_sources(&mut microphone, &mut system, &mut aligner);
            aligner.write_ready(
                microphone_writer.as_mut(),
                system_writer.as_mut(),
                Instant::now(),
                sample_limit,
            )?;
            self.emit_samples(&mut aligner);
            thread::sleep(Duration::from_millis(5));
        }

        let microphone_stats = microphone
            .take()
            .map(|capture| capture.stop(&mut aligner.microphone));
        let system_stats = system
            .take()
            .map(|capture| capture.stop(&mut aligner.system));
        aligner.flush(
            microphone_writer.as_mut(),
            system_writer.as_mut(),
            sample_limit,
        )?;
        self.emit_samples(&mut aligner);
        if let Some(writer) = microphone_writer {
            writer
                .finalize()
                .context("could not finalize microphone WAV file")?;
        }
        if let Some(writer) = system_writer {
            writer
                .finalize()
                .context("could not finalize system WAV file")?;
        }

        artifacts.write_metadata(RecordingMetadata {
            schema_version: 1,
            started_at_unix_ms,
            ended_at_unix_ms: unix_time_ms(),
            sample_rate: SAMPLE_RATE,
            channels: 1,
            bits_per_sample: 16,
            samples_written: aligner.samples_written,
            microphone: SourceMetadata::from_capture(
                self.plan.microphone_enabled(),
                MICROPHONE_FILE,
                microphone_stats,
            ),
            system: SourceMetadata::from_capture(
                self.plan.system_enabled(),
                SYSTEM_FILE,
                system_stats,
            ),
        })?;
        Ok(output)
    }

    fn emit_samples(&mut self, aligner: &mut SourceAligner) {
        for source in [RecordingSource::Microphone, RecordingSource::System] {
            let samples = aligner.take_emitted_samples(source);
            if !samples.is_empty() {
                self.sink.samples_written(source, &samples);
            }
        }
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
