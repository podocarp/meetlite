use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{bail, Context, Result};
use serde::Serialize;

use super::CaptureStatistics;

const AUDIO_FILE: &str = "audio.wav";
const METADATA_FILE: &str = "metadata.json";

#[derive(Clone)]
pub struct RecordingOutput {
    pub output_dir: PathBuf,
    pub audio_file: PathBuf,
}

pub(super) struct RecordingArtifacts {
    output_dir: PathBuf,
    audio_file: PathBuf,
}

impl RecordingArtifacts {
    pub(super) fn prepare(configured: Option<&Path>, force: bool) -> Result<Self> {
        let output_dir = output_dir(configured, force)?;
        let audio_file = output_dir.join(AUDIO_FILE);
        Ok(Self {
            output_dir,
            audio_file,
        })
    }

    pub(super) fn output(&self) -> RecordingOutput {
        RecordingOutput {
            output_dir: self.output_dir.clone(),
            audio_file: self.audio_file.clone(),
        }
    }

    pub(super) fn audio_file(&self) -> &Path {
        &self.audio_file
    }

    pub(super) fn write_metadata(&self, metadata: RecordingMetadata<'_>) -> Result<()> {
        let path = self.output_dir.join(METADATA_FILE);
        let temporary_path = self.output_dir.join("metadata.json.tmp");
        let contents = serde_json::to_vec_pretty(&metadata)?;
        fs::write(&temporary_path, contents)
            .with_context(|| format!("could not write {}", temporary_path.display()))?;
        fs::rename(&temporary_path, &path)
            .with_context(|| format!("could not atomically write {}", path.display()))
    }
}

#[derive(Serialize)]
pub(super) struct RecordingMetadata<'a> {
    pub(super) started_at_unix_ms: u128,
    pub(super) ended_at_unix_ms: u128,
    pub(super) audio_file: &'a str,
    pub(super) sample_rate: u32,
    pub(super) channels: u8,
    pub(super) bits_per_sample: u8,
    pub(super) samples_written: usize,
    pub(super) microphone: SourceMetadata,
    pub(super) system: SourceMetadata,
}

#[derive(Serialize)]
pub(super) struct SourceMetadata {
    enabled: bool,
    gain: f32,
    dropped_callback_frames: u64,
    dropped_buffered_frames: u64,
}

impl SourceMetadata {
    pub(super) fn from_capture(
        enabled: bool,
        gain: f32,
        statistics: Option<CaptureStatistics>,
    ) -> Self {
        let statistics = statistics.unwrap_or_default();
        Self {
            enabled,
            gain,
            dropped_callback_frames: statistics.dropped_callback_frames,
            dropped_buffered_frames: statistics.dropped_buffered_frames,
        }
    }
}

pub(super) fn unix_time_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time is after the Unix epoch")
        .as_millis()
}

pub(super) fn output_dir(configured: Option<&Path>, force: bool) -> Result<PathBuf> {
    let path = match configured {
        Some(path) => path.to_path_buf(),
        None => PathBuf::from(format!("meetlite-{}", unix_time_ms())),
    };
    if path.exists() {
        if !force {
            bail!(
                "refusing to use existing output directory {}; pass --force to replace Meetlite artifacts",
                path.display()
            )
        }
        for name in [
            AUDIO_FILE,
            METADATA_FILE,
            "metadata.json.tmp",
            "transcript.json",
            "transcript.jsonl",
            "summary.md",
        ] {
            let artifact = path.join(name);
            if artifact.exists() {
                fs::remove_file(&artifact)
                    .with_context(|| format!("could not remove {}", artifact.display()))?;
            }
        }
        let chunks = path.join("chunks");
        if chunks.exists() {
            fs::remove_dir_all(&chunks)
                .with_context(|| format!("could not remove {}", chunks.display()))?;
        }
    } else {
        fs::create_dir(&path)
            .with_context(|| format!("could not create output directory {}", path.display()))?;
    }
    Ok(path)
}

pub(super) fn audio_file_name() -> &'static str {
    AUDIO_FILE
}
