mod openai_compatible;

use std::{
    fs,
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    thread,
    time::Duration,
};

use anyhow::{bail, Context, Result};
use crossbeam_channel::{bounded, Receiver, Select, Sender};
use serde::{Deserialize, Serialize};

use crate::{
    cli::CaptureArgs,
    config::{RecordingConfig, SttConfig},
    credentials::Credentials,
    live_control::{LifecyclePhase, LiveControl},
    output::Output,
    recording::{self, RecordingOutput, RecordingSource},
};

const TRANSCRIPT_FILE: &str = "transcript.json";
const SAMPLE_RATE: usize = 48_000;
const LIVE_CHUNK_SAMPLES: usize = 15 * SAMPLE_RATE;
const OFFLINE_CHUNK_SAMPLES: usize = 30 * SAMPLE_RATE;
const MAX_CHUNK_DELAY_SAMPLES: usize = 5 * SAMPLE_RATE;
const SILENCE_WINDOW_SAMPLES: usize = 960;
const MIN_SILENCE_WINDOWS: usize = 15;
const QUIET_RMS: f64 = 0.01;
const ROLLING_PROMPT_CHARS: usize = 800;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transcript {
    pub schema_version: u8,
    pub text: String,
    pub language: Option<String>,
    pub duration_seconds: Option<f64>,
    pub segments: Vec<TranscriptSegment>,
    pub provider: String,
    pub model: String,
    pub source_path: String,
    pub raw_response: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TranscriptSegment {
    pub start_seconds: f64,
    pub end_seconds: f64,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speaker: Option<String>,
}

#[derive(Debug)]
pub struct TranscriptionOutput {
    pub transcript_path: PathBuf,
    pub has_text: bool,
    pub cancelled: bool,
}

pub fn transcribe_file(
    input: &Path,
    output_directory: Option<&Path>,
    config: &SttConfig,
    force: bool,
    output: Output,
) -> Result<TranscriptionOutput> {
    let directory = output_directory.map(Path::to_path_buf).unwrap_or_else(|| {
        if input.is_dir() {
            input.to_path_buf()
        } else {
            input
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or(Path::new("."))
                .to_path_buf()
        }
    });
    fs::create_dir_all(&directory)?;
    let transcript_path = directory.join(TRANSCRIPT_FILE);
    if transcript_path.exists() && !force {
        bail!(
            "refusing to overwrite {}; pass --force to replace it",
            transcript_path.display()
        )
    }
    let tracks = source_tracks(input)?;
    let legacy_audio = input
        .is_dir()
        .then(|| input.join("audio.wav"))
        .filter(|path| path.is_file());
    let credentials = Credentials::for_stt(&config.auth)?;
    let transcript = if let Some(tracks) = tracks {
        transcribe_source_tracks(tracks, config, &credentials, input)?
    } else {
        transcribe_offline(
            legacy_audio.as_deref().unwrap_or(input),
            config,
            &credentials,
        )?
    };
    write_json(&transcript_path, &transcript)?;
    emit_completed(output, &transcript, &transcript_path, true)?;
    Ok(TranscriptionOutput {
        transcript_path,
        has_text: !transcript.text.trim().is_empty(),
        cancelled: false,
    })
}

#[derive(Clone)]
struct SourceTrack {
    source: RecordingSource,
    path: PathBuf,
}

fn source_tracks(input: &Path) -> Result<Option<Vec<SourceTrack>>> {
    if !input.is_dir() {
        return Ok(None);
    }
    let tracks = [RecordingSource::Microphone, RecordingSource::System]
        .into_iter()
        .filter_map(|source| {
            let path = input.join(source.file_name());
            path.is_file().then_some(SourceTrack { source, path })
        })
        .collect::<Vec<_>>();
    if tracks.is_empty() {
        if input.join("audio.wav").is_file() {
            return Ok(None);
        }
        bail!(
            "{} does not contain microphone.wav or system.wav",
            input.display()
        )
    }
    Ok(Some(tracks))
}

fn has_audible_samples(path: &Path) -> Result<bool> {
    let reader = hound::WavReader::open(path)
        .with_context(|| format!("could not read source track {}", path.display()))?;
    let mut window = Vec::with_capacity(SILENCE_WINDOW_SAMPLES);
    for sample in reader.into_samples::<i16>() {
        window.push(
            sample.with_context(|| format!("could not read samples from {}", path.display()))?,
        );
        if window.len() == SILENCE_WINDOW_SAMPLES {
            if !is_quiet(&window) {
                return Ok(true);
            }
            window.clear();
        }
    }
    Ok(!window.is_empty() && !is_quiet(&window))
}

fn transcribe_source_tracks(
    tracks: Vec<SourceTrack>,
    config: &SttConfig,
    credentials: &Credentials,
    source: &Path,
) -> Result<Transcript> {
    transcribe_source_tracks_until(tracks, config, credentials, source, || false)?
        .context("source-track transcription was cancelled without a cancellation request")
}

fn transcribe_source_tracks_until(
    tracks: Vec<SourceTrack>,
    config: &SttConfig,
    credentials: &Credentials,
    source: &Path,
    stopped: impl Fn() -> bool,
) -> Result<Option<Transcript>> {
    let mut segments = Vec::new();
    let mut transcripts = Vec::new();
    let mut duration_seconds: f64 = 0.0;
    for track in tracks {
        if stopped() {
            return Ok(None);
        }
        let track_duration = wav_duration_seconds(&track.path)?;
        duration_seconds = duration_seconds.max(track_duration);
        if !has_audible_samples(&track.path)? {
            continue;
        }
        let Some(mut transcript) =
            transcribe_offline_until(&track.path, config, credentials, &stopped)?
        else {
            return Ok(None);
        };
        ensure_segments(&mut transcript, track_duration);
        segments.extend(transcript.segments.iter().cloned().map(|mut segment| {
            segment.speaker = Some(track.source.speaker().into());
            segment
        }));
        transcripts.push(transcript);
    }
    sort_segments(&mut segments);
    Ok(Some(Transcript {
        schema_version: 1,
        text: labeled_text(&segments),
        language: transcripts.first().and_then(|item| item.language.clone()),
        duration_seconds: Some(duration_seconds),
        segments,
        provider: config.base_url.clone(),
        model: config.model.clone(),
        source_path: source.display().to_string(),
        raw_response: serde_json::Value::Array(
            transcripts
                .into_iter()
                .map(|item| item.raw_response)
                .collect(),
        ),
    }))
}

fn sort_segments(segments: &mut [TranscriptSegment]) {
    segments.sort_by(|left, right| {
        left.start_seconds
            .total_cmp(&right.start_seconds)
            .then_with(|| left.speaker.cmp(&right.speaker))
            .then_with(|| left.end_seconds.total_cmp(&right.end_seconds))
    });
}

fn ensure_segments(transcript: &mut Transcript, duration_seconds: f64) {
    if transcript.segments.is_empty() && !transcript.text.trim().is_empty() {
        transcript.segments.push(TranscriptSegment {
            start_seconds: 0.0,
            end_seconds: duration_seconds,
            text: transcript.text.trim().to_owned(),
            speaker: None,
        });
    }
}

fn wav_duration_seconds(path: &Path) -> Result<f64> {
    let reader = hound::WavReader::open(path)
        .with_context(|| format!("could not read source track {}", path.display()))?;
    Ok(reader.duration() as f64 / reader.spec().sample_rate as f64)
}

fn labeled_text(segments: &[TranscriptSegment]) -> String {
    segments
        .iter()
        .filter_map(|segment| {
            let text = segment.text.trim();
            (!text.is_empty()).then(|| match segment.speaker.as_deref() {
                Some(speaker) => format!("{speaker}: {text}"),
                None => text.to_owned(),
            })
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn transcribe_offline(
    input: &Path,
    config: &SttConfig,
    credentials: &Credentials,
) -> Result<Transcript> {
    transcribe_offline_until(input, config, credentials, &|| false)?
        .context("offline transcription was cancelled without a cancellation request")
}

fn transcribe_offline_until(
    input: &Path,
    config: &SttConfig,
    credentials: &Credentials,
    stopped: &impl Fn() -> bool,
) -> Result<Option<Transcript>> {
    let Some(samples) = read_meetlite_wav(input) else {
        return openai_compatible::transcribe_until(
            input,
            config,
            credentials,
            config.prompt.as_deref(),
            stopped,
        );
    };
    if samples.len() <= OFFLINE_CHUNK_SAMPLES {
        return openai_compatible::transcribe_until(
            input,
            config,
            credentials,
            config.prompt.as_deref(),
            stopped,
        );
    }

    let chunks = tempfile::tempdir().context("could not create temporary transcription chunks")?;
    let mut completed = Vec::new();
    let mut history = String::new();
    let mut start_sample = 0;
    let mut index = 0;
    while start_sample < samples.len() {
        if stopped() {
            return Ok(None);
        }
        let remaining = &samples[start_sample..];
        let length = silence_aware_boundary(
            remaining,
            OFFLINE_CHUNK_SAMPLES,
            OFFLINE_CHUNK_SAMPLES + MAX_CHUNK_DELAY_SAMPLES,
        )
        .unwrap_or(remaining.len());
        let path = chunks.path().join(format!("chunk-{index:06}.wav"));
        write_chunk(&path, &remaining[..length])?;
        let prompt = transcription_prompt(config, &history);
        let Some(mut transcript) = openai_compatible::transcribe_until(
            &path,
            config,
            credentials,
            prompt.as_deref(),
            stopped,
        )?
        else {
            return Ok(None);
        };
        ensure_segments(&mut transcript, length as f64 / SAMPLE_RATE as f64);
        offset_segments(
            &mut transcript.segments,
            start_sample as f64 / SAMPLE_RATE as f64,
            length as f64 / SAMPLE_RATE as f64,
        );
        append_history(&mut history, &transcript.text);
        completed.push(transcript);
        start_sample += length;
        index += 1;
    }
    Ok(Some(merge_transcripts(completed, input)))
}

fn read_meetlite_wav(input: &Path) -> Option<Vec<i16>> {
    let reader = hound::WavReader::open(input).ok()?;
    let spec = reader.spec();
    if spec.channels != 1
        || spec.sample_rate != SAMPLE_RATE as u32
        || spec.bits_per_sample != 16
        || spec.sample_format != hound::SampleFormat::Int
    {
        return None;
    }
    reader
        .into_samples::<i16>()
        .collect::<Result<Vec<_>, _>>()
        .ok()
}

fn transcription_prompt(config: &SttConfig, history: &str) -> Option<String> {
    let static_prompt = config
        .prompt
        .as_deref()
        .filter(|prompt| !prompt.trim().is_empty());
    let history = history.trim();
    match (static_prompt, history.is_empty()) {
        (None, true) => None,
        (Some(prompt), true) => Some(prompt.to_owned()),
        (None, false) => Some(history.to_owned()),
        (Some(prompt), false) => Some(format!("{prompt}\n\n{history}")),
    }
}

fn append_history(history: &mut String, text: &str) {
    if !history.is_empty() {
        history.push(' ');
    }
    history.push_str(text.trim());
    if history.chars().count() > ROLLING_PROMPT_CHARS {
        *history = history
            .chars()
            .rev()
            .take(ROLLING_PROMPT_CHARS)
            .collect::<String>()
            .chars()
            .rev()
            .collect();
    }
}

pub fn transcribe_live(
    args: CaptureArgs,
    recording_config: Option<&RecordingConfig>,
    stt: SttConfig,
    output: Output,
    control: LiveControl,
    summary_enabled: bool,
) -> Result<TranscriptionOutput> {
    // Resolve credentials before capture so Keychain prompts never arrive mid-recording.
    let credentials = Credentials::for_stt(&stt.auth)?;
    let (microphone_sender, microphone_receiver) = bounded(4);
    let (system_sender, system_receiver) = bounded(4);
    let (started_sender, started_receiver) = bounded(1);
    let (result_sender, result_receiver) = bounded(1);
    let worker_output = output;
    let worker_control = control.clone();
    let worker_stt = stt.clone();
    let worker_credentials = credentials.clone();
    let worker = thread::spawn(move || {
        let result = live_worker(
            microphone_receiver,
            system_receiver,
            started_receiver,
            worker_stt,
            worker_credentials,
            worker_output,
            worker_control,
        );
        let _ = result_sender.send(result);
    });
    let next_chunk_index = Arc::new(AtomicUsize::new(0));
    let mut microphone_chunker = Chunker::new(
        microphone_sender,
        RecordingSource::Microphone,
        Arc::clone(&next_chunk_index),
    );
    let mut system_chunker = Chunker::new(
        system_sender,
        RecordingSource::System,
        Arc::clone(&next_chunk_index),
    );
    let recording_result = recording::record_with_samples(
        args,
        recording_config,
        Some(control.clone()),
        output,
        |recording| {
            if output.is_json() {
                output.event(&serde_json::json!({
                    "type": "lifecycle",
                    "phase": LifecyclePhase::RecordingStarted.as_str(),
                    "output_dir": recording.output_dir.display().to_string(),
                    "summary_enabled": summary_enabled,
                }))?;
            }
            started_sender.send(recording.clone()).map_err(|_| {
                anyhow::anyhow!("live transcription worker exited before recording started")
            })
        },
        |source, samples| match source {
            RecordingSource::Microphone => microphone_chunker.push(samples),
            RecordingSource::System => system_chunker.push(samples),
        },
    );
    drop(started_sender);
    let recording = match recording_result {
        Ok(recording) => recording,
        Err(error) => {
            control.stop_transcription();
            drop(microphone_chunker);
            drop(system_chunker);
            let _ = worker.join();
            return Err(error);
        }
    };
    control.begin_transcription();
    if output.is_json() {
        let output_dir = recording.output_dir.display().to_string();
        output.events(&[
            serde_json::json!({
                "type": "lifecycle",
                "phase": LifecyclePhase::RecordingStopped.as_str(),
                "output_dir": &output_dir,
            }),
            serde_json::json!({
                "type": "lifecycle",
                "phase": LifecyclePhase::ProcessingStarted.as_str(),
                "output_dir": &output_dir,
            }),
        ])?;
    }
    let dropped_chunks = finalize_live_chunkers(
        &control,
        &mut microphone_chunker,
        &mut system_chunker,
        || {
            output.instruction(
                "Draining transcription queue. Press Ctrl-C to stop transcribing. Press Ctrl-D to quit immediately.",
            );
        },
    )?;
    drop(microphone_chunker);
    drop(system_chunker);
    let worker_result = loop {
        if control.transcription_stopped() {
            output.instruction("Transcription stopped.");
            break snapshot_checkpoints(&control, &recording.output_dir)?;
        }
        match result_receiver.recv_timeout(Duration::from_millis(50)) {
            Ok(result) => {
                worker
                    .join()
                    .map_err(|_| anyhow::anyhow!("live transcription worker panicked"))?;
                break result?;
            }
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                worker
                    .join()
                    .map_err(|_| anyhow::anyhow!("live transcription worker panicked"))?;
                return Err(anyhow::anyhow!(
                    "live transcription worker exited without a result"
                ));
            }
        }
    };
    let dropped_failures =
        append_dropped_checkpoints(&recording.output_dir, &dropped_chunks, output)?;
    finalize_metadata(&recording.output_dir, &worker_result, dropped_failures)?;
    let checkpoint_transcript = worker_result.final_transcript(&recording.output_dir);
    let transcript_path = recording.output_dir.join(TRANSCRIPT_FILE);
    let (transcript, cancelled) =
        commit_final_transcript(&control, checkpoint_transcript, &transcript_path, output)?;
    if !output.is_json() {
        output.blank_line();
        output.status(
            "Saved recording",
            &recording.output_dir.display().to_string(),
        );
        output.status("Saved transcript", &transcript_path.display().to_string());
    }
    Ok(TranscriptionOutput {
        transcript_path,
        has_text: !transcript.text.trim().is_empty(),
        cancelled,
    })
}

struct AudioChunk {
    index: usize,
    source: RecordingSource,
    start_sample: usize,
    samples: Vec<i16>,
}

struct DroppedChunk {
    index: usize,
    source: RecordingSource,
    start_sample: usize,
}

struct Chunker {
    sender: Sender<AudioChunk>,
    source: RecordingSource,
    samples: Vec<i16>,
    next_index: Arc<AtomicUsize>,
    next_start_sample: usize,
    dropped_chunks: Vec<DroppedChunk>,
}

impl Chunker {
    fn new(
        sender: Sender<AudioChunk>,
        source: RecordingSource,
        next_index: Arc<AtomicUsize>,
    ) -> Self {
        Self {
            sender,
            source,
            samples: Vec::new(),
            next_index,
            next_start_sample: 0,
            dropped_chunks: Vec::new(),
        }
    }

    fn push(&mut self, samples: &[i16]) {
        self.samples.extend_from_slice(samples);
        while let Some(length) = silence_aware_boundary(
            &self.samples,
            LIVE_CHUNK_SAMPLES,
            LIVE_CHUNK_SAMPLES + MAX_CHUNK_DELAY_SAMPLES,
        ) {
            let chunk = self.samples.drain(..length).collect();
            self.send(chunk);
        }
    }
    fn finish(&mut self) {
        if !self.samples.is_empty() {
            let samples = std::mem::take(&mut self.samples);
            self.enqueue(samples);
        }
    }

    fn send(&mut self, samples: Vec<i16>) {
        self.enqueue(samples);
    }

    fn enqueue(&mut self, samples: Vec<i16>) {
        let start_sample = self.next_start_sample;
        self.next_start_sample += samples.len();
        if samples.chunks(SILENCE_WINDOW_SAMPLES).all(is_quiet) {
            return;
        }
        let index = self.next_index.fetch_add(1, Ordering::Relaxed);
        let chunk = AudioChunk {
            index,
            source: self.source,
            start_sample,
            samples,
        };
        if self.sender.try_send(chunk).is_err() {
            self.dropped_chunks.push(DroppedChunk {
                index,
                source: self.source,
                start_sample,
            });
        }
    }
}

fn finalize_live_chunkers(
    control: &LiveControl,
    microphone: &mut Chunker,
    system: &mut Chunker,
    on_finalize: impl FnOnce(),
) -> Result<Vec<DroppedChunk>> {
    control.at_commit_boundary(|| {
        if control.transcription_stopped() {
            microphone.samples.clear();
            system.samples.clear();
        } else {
            on_finalize();
            microphone.finish();
            system.finish();
        }
        let mut dropped = std::mem::take(&mut microphone.dropped_chunks);
        dropped.extend(std::mem::take(&mut system.dropped_chunks));
        Ok(dropped)
    })
}

#[derive(Serialize, Deserialize)]
struct Checkpoint {
    chunk_index: usize,
    #[serde(default)]
    source: Option<RecordingSource>,
    start_seconds: f64,
    status: String,
    transcript: Option<Transcript>,
    error: Option<String>,
}

struct WorkerResult {
    completed: Vec<Transcript>,
    failed_chunks: usize,
}

impl WorkerResult {
    fn from_checkpoints(output_dir: &Path) -> Result<Self> {
        let path = output_dir.join("transcript.jsonl");
        let contents = match fs::read_to_string(&path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(error) => {
                return Err(error).with_context(|| format!("could not read {}", path.display()))
            }
        };
        let lines = contents.lines().collect::<Vec<_>>();
        let mut checkpoints: Vec<Checkpoint> = Vec::new();
        for (index, line) in lines.iter().enumerate() {
            match serde_json::from_str(line) {
                Ok(checkpoint) => checkpoints.push(checkpoint),
                Err(_) if index + 1 == lines.len() && !contents.ends_with('\n') => {}
                Err(error) => return Err(error.into()),
            }
        }
        let completed = checkpoints
            .iter()
            .filter_map(|checkpoint| checkpoint.transcript.clone())
            .collect();
        let failed_chunks = checkpoints
            .iter()
            .filter(|checkpoint| checkpoint.status == "failed")
            .count();
        Ok(Self {
            completed,
            failed_chunks,
        })
    }

    fn final_transcript(&self, source: &Path) -> Transcript {
        merge_transcripts(self.completed.clone(), source)
    }
}

fn merge_transcripts(completed: Vec<Transcript>, source: &Path) -> Transcript {
    let mut segments = completed
        .iter()
        .flat_map(|item| item.segments.clone())
        .collect::<Vec<_>>();
    sort_segments(&mut segments);
    let text = if segments.iter().any(|segment| segment.speaker.is_some()) {
        labeled_text(&segments)
    } else {
        completed
            .iter()
            .map(|item| item.text.trim())
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    };
    let raw_response = serde_json::Value::Array(
        completed
            .iter()
            .map(|item| item.raw_response.clone())
            .collect(),
    );
    Transcript {
        schema_version: 1,
        text,
        language: completed.first().and_then(|item| item.language.clone()),
        duration_seconds: segments
            .iter()
            .map(|segment| segment.end_seconds)
            .reduce(f64::max),
        segments,
        provider: completed
            .first()
            .map(|item| item.provider.clone())
            .unwrap_or_default(),
        model: completed
            .first()
            .map(|item| item.model.clone())
            .unwrap_or_default(),
        source_path: source.display().to_string(),
        raw_response,
    }
}

fn silence_aware_boundary(samples: &[i16], target: usize, maximum: usize) -> Option<usize> {
    let available = samples.len().min(maximum);
    if available < target {
        return None;
    }
    let mut start = target;
    while start + MIN_SILENCE_WINDOWS * SILENCE_WINDOW_SAMPLES <= available {
        let end = start + MIN_SILENCE_WINDOWS * SILENCE_WINDOW_SAMPLES;
        if samples[start..end]
            .chunks_exact(SILENCE_WINDOW_SAMPLES)
            .all(is_quiet)
        {
            return Some(start);
        }
        start += SILENCE_WINDOW_SAMPLES;
    }
    (samples.len() >= maximum).then_some(maximum)
}

fn is_quiet(samples: &[i16]) -> bool {
    let mean_square = samples
        .iter()
        .map(|sample| (*sample as f64 / i16::MAX as f64).powi(2))
        .sum::<f64>()
        / samples.len() as f64;
    mean_square.sqrt() <= QUIET_RMS
}

fn commit_after_request(
    control: &LiveControl,
    commit: impl FnOnce() -> Result<()>,
) -> Result<bool> {
    control.at_commit_boundary(|| {
        if control.transcription_stopped() {
            return Ok(false);
        }
        commit()?;
        Ok(true)
    })
}

fn snapshot_checkpoints(control: &LiveControl, output_dir: &Path) -> Result<WorkerResult> {
    control.at_commit_boundary(|| WorkerResult::from_checkpoints(output_dir))
}

fn commit_final_transcript(
    control: &LiveControl,
    transcript: Transcript,
    transcript_path: &Path,
    output: Output,
) -> Result<(Transcript, bool)> {
    control.at_commit_boundary(|| {
        let cancelled = control.transcription_stopped();
        write_json(transcript_path, &transcript)?;
        emit_completed(output, &transcript, transcript_path, false)?;
        Ok((transcript, cancelled))
    })
}

fn live_worker(
    microphone_receiver: Receiver<AudioChunk>,
    system_receiver: Receiver<AudioChunk>,
    started: Receiver<RecordingOutput>,
    config: SttConfig,
    credentials: Credentials,
    output: Output,
    control: LiveControl,
) -> Result<WorkerResult> {
    let recording = started
        .recv()
        .context("recorder did not provide an output directory")?;
    let chunks = recording.output_dir.join("chunks");
    fs::create_dir_all(&chunks)?;
    let mut checkpoints = OpenOptions::new()
        .create(true)
        .append(true)
        .open(recording.output_dir.join("transcript.jsonl"))?;
    let mut completed = Vec::new();
    let mut microphone_history = String::new();
    let mut system_history = String::new();
    let mut failed_chunks = 0;
    let mut microphone_open = true;
    let mut system_open = true;
    while microphone_open || system_open {
        let mut select = Select::new();
        let microphone_operation = microphone_open.then(|| select.recv(&microphone_receiver));
        let system_operation = system_open.then(|| select.recv(&system_receiver));
        let operation = select.select();
        let index = operation.index();
        let chunk = if microphone_operation == Some(index) {
            match operation.recv(&microphone_receiver) {
                Ok(chunk) => chunk,
                Err(_) => {
                    microphone_open = false;
                    continue;
                }
            }
        } else if system_operation == Some(index) {
            match operation.recv(&system_receiver) {
                Ok(chunk) => chunk,
                Err(_) => {
                    system_open = false;
                    continue;
                }
            }
        } else {
            continue;
        };
        if control.transcription_stopped() {
            break;
        }
        if chunk.samples.chunks(SILENCE_WINDOW_SAMPLES).all(is_quiet) {
            continue;
        }
        let path = chunks.join(format!(
            "{}-{:06}.wav",
            chunk.source.file_name().trim_end_matches(".wav"),
            chunk.index
        ));
        write_chunk(&path, &chunk.samples)?;
        let start_seconds = chunk.start_sample as f64 / 48_000.0;
        let chunk_duration_seconds = chunk.samples.len() as f64 / 48_000.0;
        let history = match chunk.source {
            RecordingSource::Microphone => &microphone_history,
            RecordingSource::System => &system_history,
        };
        let prompt = transcription_prompt(&config, history);
        let transcription = openai_compatible::transcribe_until(
            &path,
            &config,
            &credentials,
            prompt.as_deref(),
            &|| control.transcription_stopped(),
        );
        let committed = match transcription {
            Ok(Some(mut transcript)) => {
                ensure_segments(&mut transcript, chunk_duration_seconds);
                offset_segments(
                    &mut transcript.segments,
                    start_seconds,
                    chunk_duration_seconds,
                );
                for segment in &mut transcript.segments {
                    segment.speaker = Some(chunk.source.speaker().into());
                }
                commit_after_request(&control, || {
                    write_checkpoint(
                        &mut checkpoints,
                        Checkpoint {
                            chunk_index: chunk.index,
                            source: Some(chunk.source),
                            start_seconds,
                            status: "completed".into(),
                            transcript: Some(transcript.clone()),
                            error: None,
                        },
                    )?;
                    emit_chunk(
                        output,
                        chunk.index,
                        chunk.source,
                        start_seconds,
                        &transcript.text,
                    )?;
                    match chunk.source {
                        RecordingSource::Microphone => {
                            append_history(&mut microphone_history, &transcript.text)
                        }
                        RecordingSource::System => {
                            append_history(&mut system_history, &transcript.text)
                        }
                    }
                    completed.push(transcript);
                    Ok(())
                })?
            }
            Ok(None) => break,
            Err(error) => {
                let error = error.to_string();
                commit_after_request(&control, || {
                    write_checkpoint(
                        &mut checkpoints,
                        Checkpoint {
                            chunk_index: chunk.index,
                            source: Some(chunk.source),
                            start_seconds,
                            status: "failed".into(),
                            transcript: None,
                            error: Some(error.clone()),
                        },
                    )?;
                    emit_failure(output, chunk.index, chunk.source, start_seconds, &error)?;
                    failed_chunks += 1;
                    Ok(())
                })?
            }
        };
        if !committed {
            break;
        }
    }
    Ok(WorkerResult {
        completed,
        failed_chunks,
    })
}

fn emit_chunk(
    output: Output,
    chunk_index: usize,
    source: RecordingSource,
    start_seconds: f64,
    text: &str,
) -> Result<()> {
    if output.is_json() {
        output.event(&serde_json::json!({
            "type": "transcription_chunk",
            "chunk_index": chunk_index,
            "source": source,
            "start_seconds": start_seconds,
            "text": text.trim(),
        }))
    } else {
        output.line(&format!(
            "[{start_seconds:>8.2}s] {}: {}",
            source.speaker(),
            text.trim()
        ))
    }
}

fn emit_failure(
    output: Output,
    chunk_index: usize,
    source: RecordingSource,
    start_seconds: f64,
    error: &str,
) -> Result<()> {
    if output.is_json() {
        output.event(&serde_json::json!({
            "type": "transcription_chunk_failed",
            "chunk_index": chunk_index,
            "source": source,
            "start_seconds": start_seconds,
            "error": error,
        }))
    } else {
        output.status(
            "Transcription failed",
            &format!(
                "{} chunk {chunk_index} at {start_seconds:.2}s: {error}",
                source.speaker()
            ),
        );
        Ok(())
    }
}

fn emit_completed(
    output: Output,
    transcript: &Transcript,
    transcript_path: &Path,
    print_text: bool,
) -> Result<()> {
    if output.is_json() {
        output.event(&serde_json::json!({
            "type": "transcription_completed",
            "transcript_path": transcript_path,
            "transcript": transcript,
        }))
    } else if print_text && !transcript.text.trim().is_empty() {
        output.line(transcript.text.trim_end())
    } else {
        Ok(())
    }
}

fn offset_segments(segments: &mut [TranscriptSegment], chunk_start: f64, chunk_duration: f64) {
    for segment in segments {
        let start = segment.start_seconds.clamp(0.0, chunk_duration);
        let end = segment.end_seconds.clamp(start, chunk_duration);
        segment.start_seconds = chunk_start + start;
        segment.end_seconds = chunk_start + end;
    }
}

fn write_chunk(path: &Path, samples: &[i16]) -> Result<()> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 48_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec)?;
    for sample in samples {
        writer.write_sample(*sample)?;
    }
    writer.finalize()?;
    Ok(())
}

fn write_checkpoint(file: &mut fs::File, checkpoint: Checkpoint) -> Result<()> {
    serde_json::to_writer(&mut *file, &checkpoint)?;
    file.write_all(b"\n")?;
    file.flush()?;
    Ok(())
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let temporary: PathBuf = path.with_extension("json.tmp");
    fs::write(&temporary, serde_json::to_vec_pretty(value)?)?;
    fs::rename(temporary, path)?;
    Ok(())
}

fn append_dropped_checkpoints(
    output_dir: &Path,
    dropped: &[DroppedChunk],
    output: Output,
) -> Result<usize> {
    let mut checkpoints = OpenOptions::new()
        .create(true)
        .append(true)
        .open(output_dir.join("transcript.jsonl"))?;
    for chunk in dropped {
        let start_seconds = chunk.start_sample as f64 / 48_000.0;
        let error = "upload queue was saturated; retranscribe the recording directory";
        write_checkpoint(
            &mut checkpoints,
            Checkpoint {
                chunk_index: chunk.index,
                source: Some(chunk.source),
                start_seconds,
                status: "failed".into(),
                transcript: None,
                error: Some(error.into()),
            },
        )?;
        emit_failure(output, chunk.index, chunk.source, start_seconds, error)?;
    }
    Ok(dropped.len())
}

fn finalize_metadata(
    output_dir: &Path,
    worker: &WorkerResult,
    dropped_failures: usize,
) -> Result<()> {
    let path = output_dir.join("metadata.json");
    let mut metadata: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
    metadata["live_transcription"] = serde_json::json!({
        "completed_chunks": worker.completed.len(),
        "failed_chunks": worker.failed_chunks + dropped_failures,
    });
    write_json(&path, &metadata)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AuthConfig, SttConfig};

    #[test]
    fn offset_segments_clamps_provider_timestamps_to_the_chunk() {
        let mut segments = vec![TranscriptSegment {
            start_seconds: 0.0,
            end_seconds: 10.0,
            text: "[BLANK_AUDIO]".into(),
            speaker: None,
        }];

        offset_segments(&mut segments, 15.0, 6.0);

        assert_eq!(segments[0].start_seconds, 15.0);
        assert_eq!(segments[0].end_seconds, 21.0);
    }

    #[test]
    fn discovers_source_tracks_from_a_recording_directory() {
        let directory = tempfile::tempdir().unwrap();
        write_chunk(
            &directory
                .path()
                .join(RecordingSource::Microphone.file_name()),
            &[0, 1],
        )
        .unwrap();
        write_chunk(
            &directory.path().join(RecordingSource::System.file_name()),
            &[0, 0],
        )
        .unwrap();

        let tracks = source_tracks(directory.path()).unwrap().unwrap();

        assert_eq!(tracks.len(), 2);
        assert_eq!(tracks[0].source, RecordingSource::Microphone);
        assert_eq!(tracks[1].source, RecordingSource::System);
    }

    #[test]
    fn renders_labeled_segments_in_timestamp_order() {
        let segments = vec![
            TranscriptSegment {
                start_seconds: 0.0,
                end_seconds: 1.0,
                text: "Hello.".into(),
                speaker: Some("You".into()),
            },
            TranscriptSegment {
                start_seconds: 1.0,
                end_seconds: 2.0,
                text: "Hi.".into(),
                speaker: Some("Remote".into()),
            },
        ];

        assert_eq!(labeled_text(&segments), "You: Hello.\nRemote: Hi.");
    }

    #[test]
    fn commit_boundary_orders_worker_commits_and_cancellation() {
        use std::sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        };

        let control = LiveControl::testing(1);
        let committed = Arc::new(AtomicBool::new(false));
        let (commit_started_sender, commit_started_receiver) = bounded(1);
        let (release_sender, release_receiver) = bounded(1);
        let worker_control = control.clone();
        let worker_committed = Arc::clone(&committed);
        let worker = thread::spawn(move || {
            commit_after_request(&worker_control, || {
                commit_started_sender.send(()).unwrap();
                release_receiver.recv().unwrap();
                worker_committed.store(true, Ordering::SeqCst);
                Ok(())
            })
            .unwrap()
        });
        commit_started_receiver.recv().unwrap();
        let interrupt_control = control.clone();
        let interrupt = thread::spawn(move || interrupt_control.test_interrupt());
        release_sender.send(()).unwrap();

        assert!(worker.join().unwrap());
        interrupt.join().unwrap();
        assert!(committed.load(Ordering::SeqCst));

        let committed = AtomicBool::new(false);
        assert!(!commit_after_request(&control, || {
            committed.store(true, Ordering::SeqCst);
            Ok(())
        })
        .unwrap());
        assert!(!committed.load(Ordering::SeqCst));
    }

    #[test]
    fn cancellation_linearizes_final_selection_and_write_to_checkpoints() {
        let directory = tempfile::tempdir().unwrap();
        let transcript_path = directory.path().join(TRANSCRIPT_FILE);
        let control = LiveControl::testing(1);
        control.test_interrupt();
        let checkpoint = Transcript {
            schema_version: 1,
            text: "checkpoint partial".into(),
            language: None,
            duration_seconds: None,
            segments: Vec::new(),
            provider: "test".into(),
            model: "test".into(),
            source_path: directory.path().display().to_string(),
            raw_response: serde_json::Value::Null,
        };

        let (selected, cancelled) =
            commit_final_transcript(&control, checkpoint, &transcript_path, Output::new(false))
                .unwrap();
        let written: Transcript =
            serde_json::from_slice(&fs::read(&transcript_path).unwrap()).unwrap();

        assert!(cancelled);
        assert_eq!(selected.text, "checkpoint partial");
        assert_eq!(written.text, "checkpoint partial");
    }

    #[test]
    fn partial_transcript_retains_only_completed_checkpoints() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("transcript.jsonl");
        let completed = Transcript {
            schema_version: 1,
            text: "Completed text".into(),
            language: Some("en".into()),
            duration_seconds: Some(1.0),
            segments: Vec::new(),
            provider: "test".into(),
            model: "test".into(),
            source_path: "chunk.wav".into(),
            raw_response: serde_json::Value::Null,
        };
        let mut checkpoints = fs::File::create(&path).unwrap();
        write_checkpoint(
            &mut checkpoints,
            Checkpoint {
                chunk_index: 0,
                source: Some(RecordingSource::Microphone),
                start_seconds: 0.0,
                status: "completed".into(),
                transcript: Some(completed),
                error: None,
            },
        )
        .unwrap();
        write_checkpoint(
            &mut checkpoints,
            Checkpoint {
                chunk_index: 1,
                source: Some(RecordingSource::System),
                start_seconds: 1.0,
                status: "failed".into(),
                transcript: None,
                error: Some("failed".into()),
            },
        )
        .unwrap();

        let result = WorkerResult::from_checkpoints(directory.path()).unwrap();
        let transcript = result.final_transcript(directory.path());

        assert_eq!(result.completed.len(), 1);
        assert_eq!(result.failed_chunks, 1);
        assert_eq!(transcript.text, "Completed text");
    }

    #[test]
    fn prefers_a_silent_boundary_after_the_target_duration() {
        let target = 10 * SILENCE_WINDOW_SAMPLES;
        let mut samples = vec![i16::MAX; target];
        samples.extend(std::iter::repeat_n(
            0,
            MIN_SILENCE_WINDOWS * SILENCE_WINDOW_SAMPLES,
        ));

        assert_eq!(
            silence_aware_boundary(&samples, target, target + 20 * SILENCE_WINDOW_SAMPLES),
            Some(target)
        );
    }

    #[test]
    fn waits_for_the_maximum_duration_before_forcing_a_boundary() {
        let target = 10 * SILENCE_WINDOW_SAMPLES;
        let maximum = target + 5 * SILENCE_WINDOW_SAMPLES;
        let samples = vec![i16::MAX; maximum - 1];
        assert_eq!(silence_aware_boundary(&samples, target, maximum), None);

        let samples = vec![i16::MAX; maximum];
        assert_eq!(
            silence_aware_boundary(&samples, target, maximum),
            Some(maximum)
        );
    }

    #[test]
    fn live_chunks_are_contiguous_without_overlap() {
        let (sender, receiver) = bounded(4);
        let mut chunker = Chunker::new(
            sender,
            RecordingSource::Microphone,
            Arc::new(AtomicUsize::new(0)),
        );
        let samples = vec![i16::MAX; LIVE_CHUNK_SAMPLES * 3];

        chunker.push(&samples);
        chunker.finish();
        drop(chunker);

        let chunks = receiver.try_iter().collect::<Vec<_>>();
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].start_sample, 0);
        assert_eq!(
            chunks[0].samples.len(),
            LIVE_CHUNK_SAMPLES + MAX_CHUNK_DELAY_SAMPLES
        );
        assert_eq!(chunks[1].start_sample, chunks[0].samples.len());
        assert_eq!(
            chunks[1].samples.len(),
            LIVE_CHUNK_SAMPLES + MAX_CHUNK_DELAY_SAMPLES
        );
        assert_eq!(
            chunks[2].start_sample,
            chunks[0].samples.len() + chunks[1].samples.len()
        );
        assert_eq!(
            chunks
                .iter()
                .map(|chunk| chunk.samples.len())
                .sum::<usize>(),
            samples.len()
        );
    }

    #[test]
    fn final_live_chunk_never_waits_for_queue_capacity() {
        let (sender, receiver) = bounded(0);
        let mut chunker = Chunker::new(
            sender,
            RecordingSource::Microphone,
            Arc::new(AtomicUsize::new(0)),
        );
        chunker.push(&vec![i16::MAX; SILENCE_WINDOW_SAMPLES]);

        chunker.finish();

        assert!(receiver.is_empty());
        assert_eq!(chunker.dropped_chunks.len(), 1);
        assert_eq!(chunker.dropped_chunks[0].start_sample, 0);
    }

    #[test]
    fn stopped_transcription_discards_residual_chunks_without_queue_failures() {
        use std::sync::atomic::AtomicBool;

        let control = LiveControl::testing(2);
        let next_index = Arc::new(AtomicUsize::new(0));
        let (microphone_sender, microphone_receiver) = bounded(0);
        let (system_sender, system_receiver) = bounded(0);
        let mut microphone = Chunker::new(
            microphone_sender,
            RecordingSource::Microphone,
            Arc::clone(&next_index),
        );
        let mut system = Chunker::new(
            system_sender,
            RecordingSource::System,
            Arc::clone(&next_index),
        );
        microphone.push(&vec![i16::MAX; SILENCE_WINDOW_SAMPLES]);
        system.push(&vec![i16::MAX; SILENCE_WINDOW_SAMPLES]);
        let finalized = AtomicBool::new(false);

        let dropped = finalize_live_chunkers(&control, &mut microphone, &mut system, || {
            finalized.store(true, Ordering::SeqCst);
        })
        .unwrap();

        assert!(!finalized.load(Ordering::SeqCst));
        assert!(microphone.samples.is_empty());
        assert!(system.samples.is_empty());
        assert!(microphone_receiver.is_empty());
        assert!(system_receiver.is_empty());
        assert!(dropped.is_empty());
        assert_eq!(next_index.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn final_chunk_enqueue_and_cancellation_share_a_commit_boundary() {
        let control = LiveControl::testing(1);
        let next_index = Arc::new(AtomicUsize::new(0));
        let (microphone_sender, microphone_receiver) = bounded(1);
        let (system_sender, system_receiver) = bounded(1);
        let mut microphone = Chunker::new(
            microphone_sender,
            RecordingSource::Microphone,
            Arc::clone(&next_index),
        );
        let mut system = Chunker::new(
            system_sender,
            RecordingSource::System,
            Arc::clone(&next_index),
        );
        microphone.push(&vec![i16::MAX; SILENCE_WINDOW_SAMPLES]);
        system.push(&vec![i16::MAX; SILENCE_WINDOW_SAMPLES]);
        let (finalize_started_sender, finalize_started_receiver) = bounded(1);
        let (release_sender, release_receiver) = bounded(1);
        let finalizer_control = control.clone();
        let finalizer = thread::spawn(move || {
            let dropped =
                finalize_live_chunkers(&finalizer_control, &mut microphone, &mut system, || {
                    finalize_started_sender.send(()).unwrap();
                    release_receiver.recv().unwrap();
                })
                .unwrap();
            (microphone, system, dropped)
        });
        finalize_started_receiver.recv().unwrap();
        let interrupt_control = control.clone();
        let interrupt = thread::spawn(move || interrupt_control.test_interrupt());

        release_sender.send(()).unwrap();
        let (microphone, system, dropped) = finalizer.join().unwrap();
        interrupt.join().unwrap();

        assert!(control.transcription_stopped());
        assert!(microphone.samples.is_empty());
        assert!(system.samples.is_empty());
        assert!(dropped.is_empty());
        assert_eq!(microphone_receiver.len(), 1);
        assert_eq!(system_receiver.len(), 1);
    }

    #[test]
    fn quiet_chunks_are_skipped_without_losing_the_source_timeline() {
        let (sender, receiver) = bounded(4);
        let mut chunker = Chunker::new(
            sender,
            RecordingSource::System,
            Arc::new(AtomicUsize::new(0)),
        );

        chunker.push(&vec![0; LIVE_CHUNK_SAMPLES]);
        chunker.finish();
        chunker.push(&vec![i16::MAX; SILENCE_WINDOW_SAMPLES]);
        chunker.finish();
        drop(chunker);

        let chunks = receiver.try_iter().collect::<Vec<_>>();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].source, RecordingSource::System);
        assert_eq!(chunks[0].start_sample, LIVE_CHUNK_SAMPLES);
    }

    #[test]
    fn merge_orders_source_segments_by_absolute_timestamp() {
        let transcript = |speaker: &str, start_seconds: f64, text: &str| Transcript {
            schema_version: 1,
            text: text.into(),
            language: Some("en".into()),
            duration_seconds: Some(start_seconds + 1.0),
            segments: vec![TranscriptSegment {
                start_seconds,
                end_seconds: start_seconds + 1.0,
                text: text.into(),
                speaker: Some(speaker.into()),
            }],
            provider: "test".into(),
            model: "test".into(),
            source_path: "chunk.wav".into(),
            raw_response: serde_json::Value::Null,
        };

        let merged = merge_transcripts(
            vec![
                transcript("Remote", 2.0, "Second"),
                transcript("You", 1.0, "First"),
            ],
            Path::new("recording"),
        );

        assert_eq!(merged.text, "You: First\nRemote: Second");
        assert_eq!(merged.segments[0].speaker.as_deref(), Some("You"));
        assert_eq!(merged.duration_seconds, Some(3.0));
    }

    #[test]
    fn rolling_prompt_combines_the_static_hint_and_completed_transcript() {
        let config = SttConfig {
            api_style: crate::config::ApiStyle::OpenAiCompatible,
            base_url: "http://127.0.0.1".into(),
            transcription_path: "/audio/transcriptions".into(),
            model: "test".into(),
            language: None,
            prompt: Some("Meetlite, PostgreSQL".into()),
            response_format: "verbose_json".into(),
            auth: AuthConfig::None,
        };
        let mut history = String::new();
        append_history(&mut history, "The Meetlite project uses PostgreSQL.");

        assert_eq!(
            transcription_prompt(&config, &history).as_deref(),
            Some("Meetlite, PostgreSQL\n\nThe Meetlite project uses PostgreSQL.")
        );
    }

    #[test]
    fn refuses_to_overwrite_transcript_without_force() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(
            directory.path().join(TRANSCRIPT_FILE),
            "existing transcript",
        )
        .unwrap();
        let config = SttConfig {
            api_style: crate::config::ApiStyle::OpenAiCompatible,
            base_url: "http://127.0.0.1:1".into(),
            transcription_path: "/audio/transcriptions".into(),
            model: "test".into(),
            language: None,
            prompt: None,
            response_format: "verbose_json".into(),
            auth: AuthConfig::None,
        };

        let error = transcribe_file(
            &directory.path().join("audio.wav"),
            Some(directory.path()),
            &config,
            false,
            Output::new(false),
        )
        .unwrap_err();
        assert!(error.to_string().contains("pass --force"));
    }
}
