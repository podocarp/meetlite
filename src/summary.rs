use std::{
    fs,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{bail, Context, Result};
use reqwest::{
    blocking::{Client, Response},
    header::{ACCEPT, CONTENT_TYPE},
};
use serde::Serialize;
use serde_json::{json, Value};

use crate::{
    config::LlmConfig, credentials::Credentials, output::Output, transcription::Transcript,
};

const SUMMARY_FILE: &str = "summary.md";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
const SUMMARY_TEMPLATE: &str = "## Summary\n\n## Decisions\n\n## Action Items\n\n## Open Questions";

#[derive(Debug, Serialize)]
pub struct SummaryOutput {
    pub summary_path: PathBuf,
    pub model: String,
    pub summary: String,
}

pub fn summarize(
    input: &Path,
    config: Option<&LlmConfig>,
    force: bool,
    output: Output,
) -> Result<SummaryOutput> {
    let config = config.context(
        "no LLM provider is configured; add an `llm` section to the Meetlite configuration",
    )?;
    let credentials = Credentials::for_llm(&config.auth)?;
    if !output.is_json() {
        output.blank_line();
        output.status("Generating", "summary...");
        output.blank_line();
    }
    let transcript: Transcript = serde_json::from_slice(
        &fs::read(input)
            .with_context(|| format!("could not read transcript {}", input.display()))?,
    )
    .with_context(|| format!("transcript {} is not valid Meetlite JSON", input.display()))?;
    if transcript.text.trim().is_empty() {
        bail!("transcript {} has no text to summarize", input.display())
    }

    let summary_path = input
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(SUMMARY_FILE);
    if summary_path.exists() && !force {
        bail!(
            "refusing to overwrite {}; pass --force to replace it",
            summary_path.display()
        )
    }
    let summary = request_summary(&transcript.text, config, &credentials, output)?;
    if summary.trim().is_empty() {
        bail!("LLM response did not include a summary")
    }
    if !output.is_json() && !summary.ends_with('\n') {
        output.line("")?;
    }
    let summary = format!("{}\n", summary.trim_end());
    fs::write(&summary_path, &summary)
        .with_context(|| format!("could not write {}", summary_path.display()))?;
    let result = SummaryOutput {
        summary_path,
        model: config.model.clone(),
        summary,
    };
    emit_completed(output, &result)?;
    Ok(result)
}

fn request_summary(
    transcript: &str,
    config: &LlmConfig,
    credentials: &Credentials,
    output: Output,
) -> Result<String> {
    let endpoint = format!(
        "{}{}",
        config.base_url.trim_end_matches('/'),
        config.chat_completions_path
    );
    let instructions = config.instructions.as_deref().unwrap_or_default();
    let body = json!({
        "model": config.model,
        "messages": [
            {"role": "system", "content": format!("You summarize meeting transcripts. Correct obvious transcription errors, but do not invent facts. Return Markdown only using this template:\n\n{SUMMARY_TEMPLATE}")},
            {"role": "user", "content": format!("{instructions}\n\nTranscript:\n{transcript}")}
        ],
        "stream": true
    });
    let client = Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()
        .context("could not create summary HTTP client")?;
    let request = credentials.apply(
        client
            .post(&endpoint)
            .header(ACCEPT, "text/event-stream")
            .json(&body),
    );
    let response = request
        .send()
        .with_context(|| format!("summary request to {endpoint} failed"))?;
    let status = response.status();
    if !status.is_success() {
        bail!("summary request to {endpoint} failed with HTTP {status}")
    }
    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    if content_type.starts_with("text/event-stream") {
        read_stream(response, output)
    } else {
        let response: Value = response
            .json()
            .context("summary provider returned invalid JSON")?;
        let summary = response
            .pointer("/choices/0/message/content")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .context("summary provider response did not include choices[0].message.content")?;
        emit_delta(output, &summary)?;
        Ok(summary)
    }
}

fn read_stream(response: Response, output: Output) -> Result<String> {
    let mut reader = BufReader::new(response);
    let mut summary = String::new();
    let mut data = Vec::new();
    loop {
        let mut line = String::new();
        let bytes = reader
            .read_line(&mut line)
            .context("could not read summary stream")?;
        if bytes == 0 {
            if !data.is_empty() {
                if process_event(&data.join("\n"), &mut summary, output)? {
                    break;
                }
            }
            break;
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            if !data.is_empty() && process_event(&data.join("\n"), &mut summary, output)? {
                break;
            }
            data.clear();
        } else if let Some(value) = line.strip_prefix("data:") {
            data.push(value.strip_prefix(' ').unwrap_or(value).to_owned());
        }
    }
    Ok(summary)
}

fn process_event(data: &str, summary: &mut String, output: Output) -> Result<bool> {
    if data == "[DONE]" {
        return Ok(true);
    }
    let event: Value =
        serde_json::from_str(data).context("summary provider returned invalid SSE data")?;
    if let Some(delta) = event
        .pointer("/choices/0/delta/content")
        .and_then(Value::as_str)
        .filter(|delta| !delta.is_empty())
    {
        summary.push_str(delta);
        emit_delta(output, delta)?;
    }
    Ok(false)
}

fn emit_delta(output: Output, delta: &str) -> Result<()> {
    if output.is_json() {
        output.event(&json!({"type": "summary_delta", "text": delta}))
    } else {
        output.fragment(delta)
    }
}

fn emit_completed(output: Output, result: &SummaryOutput) -> Result<()> {
    if output.is_json() {
        output.event(&json!({
            "type": "summary_completed",
            "summary_path": result.summary_path,
            "model": result.model,
            "summary": result.summary,
        }))
    } else {
        output.status("Saved summary", &result.summary_path.display().to_string());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AuthConfig;
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
    };

    #[test]
    fn writes_markdown_summary_from_openai_compatible_response() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut buffer = [0; 4096];
            loop {
                let bytes_read = stream.read(&mut buffer).unwrap();
                request.extend_from_slice(&buffer[..bytes_read]);
                let Some(headers_end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n")
                else {
                    continue;
                };
                let headers = String::from_utf8_lossy(&request[..headers_end]).to_ascii_lowercase();
                let content_length = headers
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length: "))
                    .unwrap()
                    .parse::<usize>()
                    .unwrap();
                if request.len() >= headers_end + 4 + content_length {
                    break;
                }
            }
            let request = String::from_utf8_lossy(&request);
            assert!(request.starts_with("POST /v1/chat/completions HTTP/1.1"));
            assert!(request.contains("DeepSeek-V4-Pro"));
            assert!(request.contains("Correct Acme to Acme Corp"));
            assert!(request.contains("\"stream\":true"));
            let body = r###"{"choices":[{"message":{"content":"## Summary\nAcme Corp met."}}]}"###;
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
        });

        let directory = tempfile::tempdir().unwrap();
        let transcript_path = directory.path().join("transcript.json");
        fs::write(
            &transcript_path,
            serde_json::to_vec(&Transcript {
                schema_version: 1,
                text: "Acme met.".into(),
                language: None,
                duration_seconds: None,
                segments: Vec::new(),
                provider: "test".into(),
                model: "test".into(),
                source_path: "test.wav".into(),
                raw_response: Value::Null,
            })
            .unwrap(),
        )
        .unwrap();
        let config = LlmConfig {
            api_style: crate::config::ApiStyle::OpenAiCompatible,
            base_url: format!("http://{address}/v1"),
            chat_completions_path: "/chat/completions".into(),
            model: "DeepSeek-V4-Pro".into(),
            auth: AuthConfig::None,
            instructions: Some("Correct Acme to Acme Corp".into()),
        };

        let output = summarize(&transcript_path, Some(&config), false, Output::new(false)).unwrap();
        server.join().unwrap();
        assert_eq!(output.summary_path, directory.path().join(SUMMARY_FILE));
        assert_eq!(
            fs::read_to_string(output.summary_path).unwrap(),
            "## Summary\nAcme Corp met.\n"
        );
    }

    #[test]
    fn streams_openai_compatible_summary_deltas() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut buffer = [0; 4096];
            loop {
                let bytes_read = stream.read(&mut buffer).unwrap();
                request.extend_from_slice(&buffer[..bytes_read]);
                let Some(headers_end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n")
                else {
                    continue;
                };
                let headers = String::from_utf8_lossy(&request[..headers_end]).to_ascii_lowercase();
                let content_length = headers
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length: "))
                    .unwrap()
                    .parse::<usize>()
                    .unwrap();
                if request.len() >= headers_end + 4 + content_length {
                    break;
                }
            }
            let request = String::from_utf8_lossy(&request);
            assert!(request.contains("accept: text/event-stream"));
            assert!(request.contains("\"stream\":true"));
            let body = concat!(
                "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\"}}]}\n\n",
                "data: {\"choices\":[{\"delta\":{\"content\":\"## Summary\\n\"}}]}\n\n",
                "data: {\"choices\":[{\"delta\":{\"content\":\"Streamed.\"}}]}\n\n",
                "data: [DONE]\n\n"
            );
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
        });

        let directory = tempfile::tempdir().unwrap();
        let transcript_path = directory.path().join("transcript.json");
        fs::write(&transcript_path, r#"{"schema_version":1,"text":"Meeting text","language":null,"duration_seconds":null,"segments":[],"provider":"test","model":"test","source_path":"test.wav","raw_response":null}"#).unwrap();
        let config = LlmConfig {
            api_style: crate::config::ApiStyle::OpenAiCompatible,
            base_url: format!("http://{address}/v1"),
            chat_completions_path: "/chat/completions".into(),
            model: "test".into(),
            auth: AuthConfig::None,
            instructions: None,
        };

        let output = summarize(&transcript_path, Some(&config), false, Output::new(false)).unwrap();
        server.join().unwrap();
        assert_eq!(output.summary, "## Summary\nStreamed.\n");
        assert_eq!(
            fs::read_to_string(output.summary_path).unwrap(),
            "## Summary\nStreamed.\n"
        );
    }

    #[test]
    fn refuses_to_overwrite_summary_without_force() {
        let directory = tempfile::tempdir().unwrap();
        let transcript_path = directory.path().join("transcript.json");
        fs::write(&transcript_path, r#"{"schema_version":1,"text":"Meeting text","language":null,"duration_seconds":null,"segments":[],"provider":"test","model":"test","source_path":"test.wav","raw_response":null}"#).unwrap();
        fs::write(directory.path().join(SUMMARY_FILE), "existing summary").unwrap();
        let config = LlmConfig {
            api_style: crate::config::ApiStyle::OpenAiCompatible,
            base_url: "http://127.0.0.1:1".into(),
            chat_completions_path: "/chat/completions".into(),
            model: "test".into(),
            auth: AuthConfig::None,
            instructions: None,
        };

        let error =
            summarize(&transcript_path, Some(&config), false, Output::new(false)).unwrap_err();
        assert!(error.to_string().contains("pass --force"));
        assert_eq!(
            fs::read_to_string(directory.path().join(SUMMARY_FILE)).unwrap(),
            "existing summary"
        );
    }
}
