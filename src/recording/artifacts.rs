use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{bail, Context, Result};
use serde::Serialize;
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

use super::CaptureStatistics;

pub(crate) const MICROPHONE_FILE: &str = "microphone.wav";
pub(crate) const SYSTEM_FILE: &str = "system.wav";
const METADATA_FILE: &str = "metadata.json";

#[derive(Clone)]
pub struct RecordingOutput {
    pub output_dir: PathBuf,
}

pub(super) struct RecordingArtifacts {
    output_dir: PathBuf,
    microphone_file: Option<PathBuf>,
    system_file: Option<PathBuf>,
}

impl RecordingArtifacts {
    pub(super) fn prepare(
        configured: Option<&Path>,
        force: bool,
        microphone_enabled: bool,
        system_enabled: bool,
    ) -> Result<Self> {
        let output_dir = output_dir(configured, force)?;
        let microphone_file = microphone_enabled.then(|| output_dir.join(MICROPHONE_FILE));
        let system_file = system_enabled.then(|| output_dir.join(SYSTEM_FILE));
        Ok(Self {
            output_dir,
            microphone_file,
            system_file,
        })
    }

    pub(super) fn output(&self) -> RecordingOutput {
        RecordingOutput {
            output_dir: self.output_dir.clone(),
        }
    }

    pub(super) fn microphone_file(&self) -> Option<&Path> {
        self.microphone_file.as_deref()
    }

    pub(super) fn system_file(&self) -> Option<&Path> {
        self.system_file.as_deref()
    }

    pub(super) fn write_metadata(&self, metadata: RecordingMetadata) -> Result<()> {
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
pub(super) struct RecordingMetadata {
    pub(super) schema_version: u8,
    pub(super) started_at_unix_ms: u128,
    pub(super) ended_at_unix_ms: u128,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    file: Option<&'static str>,
    dropped_callback_frames: u64,
    dropped_buffered_frames: u64,
}

impl SourceMetadata {
    pub(super) fn from_capture(
        enabled: bool,
        file: &'static str,
        statistics: Option<CaptureStatistics>,
    ) -> Self {
        let statistics = statistics.unwrap_or_default();
        Self {
            enabled,
            file: enabled.then_some(file),
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
        None => PathBuf::from(format!("meetlite-{}", recording_timestamp()?)),
    };
    if path.exists() {
        if !force {
            bail!(
                "refusing to use existing output directory {}; pass --force to replace Meetlite artifacts",
                path.display()
            )
        }
        for name in [
            MICROPHONE_FILE,
            SYSTEM_FILE,
            METADATA_FILE,
            "metadata.json.tmp",
            "transcript.json",
            "transcript.json.tmp",
            "transcript.jsonl",
            "summary.md",
            "summary.md.tmp",
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

fn recording_timestamp() -> Result<String> {
    let timestamp = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .context("could not format recording timestamp")?;
    Ok(timestamp.replace(':', "-"))
}
