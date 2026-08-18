mod openai_compatible;

use std::{fs, path::Path};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::SttConfig;

const TRANSCRIPT_FILE: &str = "transcript.json";

#[derive(Debug, Serialize, Deserialize)]
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

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct TranscriptSegment {
    pub start_seconds: f64,
    pub end_seconds: f64,
    pub text: String,
}

pub fn transcribe_file(
    input: &Path,
    output: Option<&Path>,
    config: &SttConfig,
) -> Result<Transcript> {
    let transcript = openai_compatible::transcribe(input, config)?;
    let directory = output
        .map(Path::to_path_buf)
        .or_else(|| input.parent().map(Path::to_path_buf))
        .context("input audio path must include a parent directory")?;
    fs::create_dir_all(&directory).with_context(|| {
        format!(
            "could not create transcript directory {}",
            directory.display()
        )
    })?;
    let destination = directory.join(TRANSCRIPT_FILE);
    let temporary = directory.join(format!("{TRANSCRIPT_FILE}.tmp"));
    fs::write(&temporary, serde_json::to_vec_pretty(&transcript)?)
        .with_context(|| format!("could not write {}", temporary.display()))?;
    fs::rename(&temporary, &destination)
        .with_context(|| format!("could not atomically write {}", destination.display()))?;
    Ok(transcript)
}
