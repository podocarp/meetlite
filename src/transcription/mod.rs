mod openai_compatible;

use std::{
    fs,
    fs::OpenOptions,
    io::{BufRead, Write},
    path::{Path, PathBuf},
    thread,
    time::Duration,
};

use anyhow::{bail, Context, Result};
use crossbeam_channel::{bounded, Receiver, Sender};
use serde::{Deserialize, Serialize};

use crate::{
    cli::CaptureArgs,
    config::{RecordingConfig, SttConfig},
    credentials::Credentials,
    live_control::LiveControl,
    output::Output,
    recording::{self, RecordingOutput},
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
}

#[derive(Debug)]
pub struct TranscriptionOutput {
    pub transcript_path: PathBuf,
}

pub fn transcribe_file(
    input: &Path,
    output_directory: Option<&Path>,
    config: &SttConfig,
    force: bool,
    output: Output,
) -> Result<TranscriptionOutput> {
    let directory = output_directory
        .map(Path::to_path_buf)
        .or_else(|| input.parent().map(Path::to_path_buf))
        .context("input audio path must include a parent directory")?;
    fs::create_dir_all(&directory)?;
    let transcript_path = directory.join(TRANSCRIPT_FILE);
    if transcript_path.exists() && !force {
        bail!(
            "refusing to overwrite {}; pass --force to replace it",
            transcript_path.display()
        )
    }
    let credentials = Credentials::for_stt(&config.auth)?;
    let transcript = transcribe_offline(input, config, &credentials)?;
    write_json(&transcript_path, &transcript)?;
    emit_completed(output, &transcript, &transcript_path, true)?;
    Ok(TranscriptionOutput { transcript_path })
}

fn transcribe_offline(
    input: &Path,
    config: &SttConfig,
    credentials: &Credentials,
) -> Result<Transcript> {
    let Some(samples) = read_meetlite_wav(input) else {
        return openai_compatible::transcribe(input, config, credentials, config.prompt.as_deref());
    };
    if samples.len() <= OFFLINE_CHUNK_SAMPLES {
        return openai_compatible::transcribe(input, config, credentials, config.prompt.as_deref());
    }

    let chunks = tempfile::tempdir().context("could not create temporary transcription chunks")?;
    let mut completed = Vec::new();
    let mut history = String::new();
    let mut start_sample = 0;
    let mut index = 0;
    while start_sample < samples.len() {
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
        let mut transcript =
            openai_compatible::transcribe(&path, config, credentials, prompt.as_deref())?;
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
    Ok(merge_transcripts(completed, input))
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
) -> Result<TranscriptionOutput> {
    // Resolve credentials before capture so Keychain prompts never arrive mid-recording.
    let credentials = Credentials::for_stt(&stt.auth)?;
    let (sender, receiver) = bounded(4);
    let (started_sender, started_receiver) = bounded(1);
    let (result_sender, result_receiver) = bounded(1);
    let worker_output = output;
    let worker_control = control.clone();
    let worker = thread::spawn(move || {
        let result = live_worker(
            receiver,
            started_receiver,
            stt,
            credentials,
            worker_output,
            worker_control,
        );
        let _ = result_sender.send(result);
    });
    let mut chunker = Chunker::new(sender);
    let recording_result = recording::record_with_samples(
        args,
        recording_config,
        Some(control.clone()),
        |output| {
            let _ = started_sender.send(output.clone());
        },
        |samples| chunker.push(samples),
    );
    drop(started_sender);
    chunker.finish();
    let dropped_chunks = std::mem::take(&mut chunker.dropped_chunks);
    drop(chunker);
    let recording = recording_result?;
    control.begin_transcription();
    if !control.transcription_stopped() {
        output.instruction(
            "Draining transcription queue. Press Ctrl-C to stop transcribing. Press Ctrl-D to quit immediately.",
        );
    }
    let worker_result = loop {
        if control.transcription_stopped() {
            output.instruction("Transcription stopped.");
            break WorkerResult::from_checkpoints(&recording.output_dir)?;
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
    let transcript = worker_result.final_transcript(&recording.audio_file);
    let transcript_path = recording.output_dir.join(TRANSCRIPT_FILE);
    write_json(&transcript_path, &transcript)?;
    emit_completed(output, &transcript, &transcript_path, false)?;
    if !output.is_json() {
        output.blank_line();
        output.status(
            "Saved recording",
            &recording.audio_file.display().to_string(),
        );
        output.status("Saved transcript", &transcript_path.display().to_string());
    }
    Ok(TranscriptionOutput { transcript_path })
}

struct AudioChunk {
    index: usize,
    start_sample: usize,
    samples: Vec<i16>,
}

struct Chunker {
    sender: Sender<AudioChunk>,
    samples: Vec<i16>,
    next_index: usize,
    next_start_sample: usize,
    dropped_chunks: Vec<(usize, usize)>,
}

impl Chunker {
    fn new(sender: Sender<AudioChunk>) -> Self {
        Self {
            sender,
            samples: Vec::new(),
            next_index: 0,
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
            let chunk = std::mem::take(&mut self.samples);
            self.send(chunk);
        }
    }
    fn send(&mut self, samples: Vec<i16>) {
        let sample_count = samples.len();
        let chunk = AudioChunk {
            index: self.next_index,
            start_sample: self.next_start_sample,
            samples,
        };
        self.next_index += 1;
        self.next_start_sample += sample_count;
        if self.sender.try_send(chunk).is_err() {
            self.dropped_chunks
                .push((self.next_index - 1, self.next_start_sample - sample_count));
        }
    }
}

#[derive(Serialize, Deserialize)]
struct Checkpoint {
    chunk_index: usize,
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
        let reader =
            fs::File::open(&path).with_context(|| format!("could not read {}", path.display()))?;
        let checkpoints = std::io::BufReader::new(reader)
            .lines()
            .map(|line| -> Result<Checkpoint> { Ok(serde_json::from_str(&line?)?) })
            .collect::<Result<Vec<_>>>()?;
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
    let text = completed
        .iter()
        .map(|item| item.text.trim())
        .collect::<Vec<_>>()
        .join("\n");
    let segments = completed
        .iter()
        .flat_map(|item| item.segments.clone())
        .collect();
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
        duration_seconds: None,
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

fn live_worker(
    receiver: Receiver<AudioChunk>,
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
    let mut history = String::new();
    let mut failed_chunks = 0;
    while let Ok(chunk) = receiver.recv() {
        if control.transcription_stopped() {
            break;
        }
        let path = chunks.join(format!("chunk-{:06}.wav", chunk.index));
        write_chunk(&path, &chunk.samples)?;
        let start_seconds = chunk.start_sample as f64 / 48_000.0;
        let chunk_duration_seconds = chunk.samples.len() as f64 / 48_000.0;
        let prompt = transcription_prompt(&config, &history);
        let transcription =
            openai_compatible::transcribe(&path, &config, &credentials, prompt.as_deref());
        // Do not let a request that was abandoned during its response race the final transcript.
        if control.transcription_stopped() {
            break;
        }
        match transcription {
            Ok(mut transcript) => {
                offset_segments(
                    &mut transcript.segments,
                    start_seconds,
                    chunk_duration_seconds,
                );
                write_checkpoint(
                    &mut checkpoints,
                    Checkpoint {
                        chunk_index: chunk.index,
                        start_seconds,
                        status: "completed".into(),
                        transcript: Some(transcript.clone()),
                        error: None,
                    },
                )?;
                emit_chunk(output, chunk.index, start_seconds, &transcript.text)?;
                append_history(&mut history, &transcript.text);
                completed.push(transcript);
            }
            Err(error) => {
                let error = error.to_string();
                write_checkpoint(
                    &mut checkpoints,
                    Checkpoint {
                        chunk_index: chunk.index,
                        start_seconds,
                        status: "failed".into(),
                        transcript: None,
                        error: Some(error.clone()),
                    },
                )?;
                emit_failure(output, chunk.index, start_seconds, &error)?;
                failed_chunks += 1;
            }
        }
    }
    Ok(WorkerResult {
        completed,
        failed_chunks,
    })
}

fn emit_chunk(output: Output, chunk_index: usize, start_seconds: f64, text: &str) -> Result<()> {
    if output.is_json() {
        output.event(&serde_json::json!({
            "type": "transcription_chunk",
            "chunk_index": chunk_index,
            "start_seconds": start_seconds,
            "text": text.trim(),
        }))
    } else {
        output.line(&format!("[{start_seconds:>8.2}s] {}", text.trim()))
    }
}

fn emit_failure(output: Output, chunk_index: usize, start_seconds: f64, error: &str) -> Result<()> {
    if output.is_json() {
        output.event(&serde_json::json!({
            "type": "transcription_chunk_failed",
            "chunk_index": chunk_index,
            "start_seconds": start_seconds,
            "error": error,
        }))
    } else {
        output.status(
            "Transcription failed",
            &format!("chunk {chunk_index} at {start_seconds:.2}s: {error}"),
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
    dropped: &[(usize, usize)],
    output: Output,
) -> Result<usize> {
    let mut checkpoints = OpenOptions::new()
        .append(true)
        .open(output_dir.join("transcript.jsonl"))?;
    for (chunk_index, start_sample) in dropped {
        let start_seconds = *start_sample as f64 / 48_000.0;
        let error = "upload queue was saturated; retranscribe audio.wav after recording";
        write_checkpoint(
            &mut checkpoints,
            Checkpoint {
                chunk_index: *chunk_index,
                start_seconds,
                status: "failed".into(),
                transcript: None,
                error: Some(error.into()),
            },
        )?;
        emit_failure(output, *chunk_index, start_seconds, error)?;
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
        }];

        offset_segments(&mut segments, 15.0, 6.0);

        assert_eq!(segments[0].start_seconds, 15.0);
        assert_eq!(segments[0].end_seconds, 21.0);
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
        let mut chunker = Chunker::new(sender);
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
