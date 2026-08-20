mod openai_compatible;

use std::{
    fs,
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
    thread,
};

use anyhow::{bail, Context, Result};
use crossbeam_channel::{bounded, Receiver, Sender};
use serde::{Deserialize, Serialize};

use crate::{
    cli::RecordArgs,
    config::{RecordingConfig, SttConfig},
    recording::{self, RecordingOutput},
};

const TRANSCRIPT_FILE: &str = "transcript.json";
const LIVE_CHUNK_SAMPLES: usize = 15 * 48_000;

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

pub fn transcribe_file(
    input: &Path,
    output: Option<&Path>,
    config: &SttConfig,
    force: bool,
) -> Result<Transcript> {
    let directory = output
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
    let transcript = openai_compatible::transcribe(input, config)?;
    write_json(&transcript_path, &transcript)?;
    Ok(transcript)
}

pub fn transcribe_live(
    args: RecordArgs,
    recording_config: Option<&RecordingConfig>,
    stt: SttConfig,
) -> Result<Transcript> {
    let (sender, receiver) = bounded(4);
    let (started_sender, started_receiver) = bounded(1);
    let worker = thread::spawn(move || live_worker(receiver, started_receiver, stt));
    let mut chunker = Chunker::new(sender);
    let recording_result = recording::record_with_samples(
        args,
        recording_config,
        |output| {
            let _ = started_sender.send(output.clone());
        },
        |samples| chunker.push(samples),
    );
    chunker.finish();
    let dropped_chunks = std::mem::take(&mut chunker.dropped_chunks);
    drop(chunker);
    let worker_result = worker.join().expect("live transcription worker panicked")?;
    let output = recording_result?;
    let dropped_failures = append_dropped_checkpoints(&output.output_dir, &dropped_chunks)?;
    finalize_metadata(&output.output_dir, &worker_result, dropped_failures)?;
    let transcript = worker_result.final_transcript(&output.audio_file);
    write_json(&output.output_dir.join(TRANSCRIPT_FILE), &transcript)?;
    Ok(transcript)
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
        while self.samples.len() >= LIVE_CHUNK_SAMPLES {
            let chunk = self.samples.drain(..LIVE_CHUNK_SAMPLES).collect();
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

#[derive(Serialize)]
struct Checkpoint {
    chunk_index: usize,
    start_seconds: f64,
    status: &'static str,
    transcript: Option<Transcript>,
    error: Option<String>,
}

struct WorkerResult {
    completed: Vec<Transcript>,
    failed_chunks: usize,
}

impl WorkerResult {
    fn final_transcript(&self, source: &Path) -> Transcript {
        let text = self
            .completed
            .iter()
            .map(|item| item.text.trim())
            .collect::<Vec<_>>()
            .join("\n");
        let segments = self
            .completed
            .iter()
            .flat_map(|item| item.segments.clone())
            .collect();
        let raw_response = serde_json::Value::Array(
            self.completed
                .iter()
                .map(|item| item.raw_response.clone())
                .collect(),
        );
        Transcript {
            schema_version: 1,
            text,
            language: None,
            duration_seconds: None,
            segments,
            provider: self
                .completed
                .first()
                .map(|item| item.provider.clone())
                .unwrap_or_default(),
            model: self
                .completed
                .first()
                .map(|item| item.model.clone())
                .unwrap_or_default(),
            source_path: source.display().to_string(),
            raw_response,
        }
    }
}

fn live_worker(
    receiver: Receiver<AudioChunk>,
    started: Receiver<RecordingOutput>,
    config: SttConfig,
) -> Result<WorkerResult> {
    let output = started
        .recv()
        .context("recorder did not provide an output directory")?;
    let chunks = output.output_dir.join("chunks");
    fs::create_dir_all(&chunks)?;
    let mut checkpoints = OpenOptions::new()
        .create(true)
        .append(true)
        .open(output.output_dir.join("transcript.jsonl"))?;
    let mut completed = Vec::new();
    let mut failed_chunks = 0;
    while let Ok(chunk) = receiver.recv() {
        let path = chunks.join(format!("chunk-{:06}.wav", chunk.index));
        write_chunk(&path, &chunk.samples)?;
        let start_seconds = chunk.start_sample as f64 / 48_000.0;
        let chunk_duration_seconds = chunk.samples.len() as f64 / 48_000.0;
        match openai_compatible::transcribe(&path, &config) {
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
                        status: "completed",
                        transcript: Some(transcript.clone()),
                        error: None,
                    },
                )?;
                println!("[{start_seconds:>8.2}s] {}", transcript.text.trim());
                completed.push(transcript);
            }
            Err(error) => {
                write_checkpoint(
                    &mut checkpoints,
                    Checkpoint {
                        chunk_index: chunk.index,
                        start_seconds,
                        status: "failed",
                        transcript: None,
                        error: Some(error.to_string()),
                    },
                )?;
                failed_chunks += 1;
            }
        }
    }
    Ok(WorkerResult {
        completed,
        failed_chunks,
    })
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

fn append_dropped_checkpoints(output_dir: &Path, dropped: &[(usize, usize)]) -> Result<usize> {
    let mut checkpoints = OpenOptions::new()
        .append(true)
        .open(output_dir.join("transcript.jsonl"))?;
    for (chunk_index, start_sample) in dropped {
        write_checkpoint(
            &mut checkpoints,
            Checkpoint {
                chunk_index: *chunk_index,
                start_seconds: *start_sample as f64 / 48_000.0,
                status: "failed",
                transcript: None,
                error: Some(
                    "upload queue was saturated; retranscribe audio.wav after recording".into(),
                ),
            },
        )?;
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
    fn refuses_to_overwrite_transcript_without_force() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(
            directory.path().join(TRANSCRIPT_FILE),
            "existing transcript",
        )
        .unwrap();
        let config = SttConfig {
            base_url: "http://127.0.0.1:1".into(),
            transcription_path: "/audio/transcriptions".into(),
            model: "test".into(),
            language: None,
            response_format: "verbose_json".into(),
            auth: AuthConfig::None,
        };

        let error = transcribe_file(
            &directory.path().join("audio.wav"),
            Some(directory.path()),
            &config,
            false,
        )
        .unwrap_err();
        assert!(error.to_string().contains("pass --force"));
    }
}
