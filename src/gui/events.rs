use std::path::PathBuf;

use serde::Deserialize;
use serde_json::Value;

use super::session::SessionEvent;

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum CliEvent {
    Lifecycle {
        phase: LifecyclePhase,
        #[serde(default)]
        output_dir: Option<PathBuf>,
        #[serde(default)]
        transcript_path: Option<PathBuf>,
        #[serde(default)]
        summary_enabled: Option<bool>,
    },
    TranscriptionChunk {
        chunk_index: usize,
        #[serde(default)]
        source: Option<String>,
        start_seconds: f64,
        text: String,
    },
    TranscriptionChunkFailed {
        chunk_index: usize,
        #[serde(default)]
        source: Option<String>,
        start_seconds: f64,
        error: String,
    },
    TranscriptionCompleted {
        transcript_path: PathBuf,
        transcript: Value,
    },
    SummaryDelta {
        text: String,
    },
    SummaryCompleted {
        summary_path: PathBuf,
        model: String,
        summary: String,
    },
    Error {
        message: String,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LifecyclePhase {
    RecordingStarted,
    RecordingStopped,
    ProcessingStarted,
    SummarizingStarted,
}

impl CliEvent {
    pub(crate) fn session_event(&self) -> Option<SessionEvent> {
        match self {
            Self::Lifecycle { phase, .. } => Some(match phase {
                LifecyclePhase::RecordingStarted => SessionEvent::RecordingStarted,
                LifecyclePhase::RecordingStopped => SessionEvent::RecordingStopped,
                LifecyclePhase::ProcessingStarted => SessionEvent::ProcessingStarted,
                LifecyclePhase::SummarizingStarted => SessionEvent::SummarizingStarted,
            }),
            Self::TranscriptionCompleted { .. } => Some(SessionEvent::TranscriptionCompleted),
            Self::SummaryCompleted { .. } => Some(SessionEvent::SummaryCompleted),
            Self::Error { .. } => Some(SessionEvent::ErrorReceived),
            _ => None,
        }
    }
}

pub(crate) fn parse_line(line: &str) -> Result<CliEvent, String> {
    let event: CliEvent =
        serde_json::from_str(line).map_err(|error| format!("Invalid CLI event: {error}"))?;
    if let CliEvent::Lifecycle {
        phase,
        output_dir,
        transcript_path,
        ..
    } = &event
    {
        let valid = match phase {
            LifecyclePhase::RecordingStarted
            | LifecyclePhase::RecordingStopped
            | LifecyclePhase::ProcessingStarted => output_dir.is_some(),
            LifecyclePhase::SummarizingStarted => transcript_path.is_some(),
        };
        if !valid {
            return Err(format!("Invalid CLI event: missing path for {phase:?}"));
        }
    }
    Ok(event)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_event_variants_and_ignores_unknown_fields() {
        let cases = [
            r#"{"type":"lifecycle","phase":"recording_started","output_dir":"recording","future":true}"#,
            r#"{"type":"lifecycle","phase":"recording_stopped","output_dir":"recording"}"#,
            r#"{"type":"lifecycle","phase":"processing_started","output_dir":"recording"}"#,
            r#"{"type":"lifecycle","phase":"summarizing_started","transcript_path":"transcript.json"}"#,
            r#"{"type":"transcription_chunk","chunk_index":1,"source":"microphone","start_seconds":2.5,"text":"hello"}"#,
            r#"{"type":"transcription_chunk_failed","chunk_index":2,"source":"system","start_seconds":3.5,"error":"failed"}"#,
            r#"{"type":"transcription_completed","transcript_path":"transcript.json","transcript":{"text":"hello"}}"#,
            r##"{"type":"summary_delta","text":"# Summary"}"##,
            r#"{"type":"summary_completed","summary_path":"summary.md","model":"test","summary":"done"}"#,
            r#"{"type":"error","message":"failed"}"#,
        ];

        for line in cases {
            assert!(parse_line(line).is_ok(), "{line}");
        }
    }

    #[test]
    fn maps_lifecycle_and_terminal_events_to_the_reducer() {
        let recording = parse_line(
            r#"{"type":"lifecycle","phase":"recording_started","output_dir":"recording"}"#,
        )
        .unwrap();
        assert_eq!(
            recording.session_event(),
            Some(SessionEvent::RecordingStarted)
        );

        let completed = parse_line(
            r#"{"type":"summary_completed","summary_path":"summary.md","model":"test","summary":"done"}"#,
        )
        .unwrap();
        assert_eq!(
            completed.session_event(),
            Some(SessionEvent::SummaryCompleted)
        );
    }

    #[test]
    fn rejects_invalid_protocol_lines() {
        for line in [
            "not json",
            "[]",
            r#"{"type":"future_event"}"#,
            r#"{"type":"summary_delta"}"#,
            r#"{"type":"lifecycle","phase":"future_phase"}"#,
        ] {
            assert!(parse_line(line).is_err(), "{line}");
        }
    }
}
