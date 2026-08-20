use std::{
    env, fs,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{bail, Context, Result};
use reqwest::{
    blocking::{Client, RequestBuilder},
    header::{HeaderName, HeaderValue, AUTHORIZATION},
};
use serde::Serialize;
use serde_json::{json, Value};

use crate::{
    config::{AuthConfig, LlmConfig},
    transcription::Transcript,
};

const SUMMARY_FILE: &str = "summary.md";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
const SUMMARY_TEMPLATE: &str = "## Summary\n\n## Decisions\n\n## Action Items\n\n## Open Questions";

#[derive(Debug, Serialize)]
pub struct SummaryOutput {
    pub summary_path: PathBuf,
    pub model: String,
}

pub fn summarize(input: &Path, config: Option<&LlmConfig>, force: bool) -> Result<SummaryOutput> {
    let config = config.context(
        "no LLM provider is configured; add an `llm` section to the Meetlite configuration",
    )?;
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
    let summary = request_summary(&transcript.text, config)?;
    if summary.trim().is_empty() {
        bail!("LLM response did not include a summary")
    }
    fs::write(&summary_path, format!("{}\n", summary.trim_end()))
        .with_context(|| format!("could not write {}", summary_path.display()))?;
    Ok(SummaryOutput {
        summary_path,
        model: config.model.clone(),
    })
}

fn request_summary(transcript: &str, config: &LlmConfig) -> Result<String> {
    let endpoint = format!(
        "{}{}",
        config.base_url.trim_end_matches('/'),
        config.chat_completions_path
    );
    let corrections = config.instructions.as_deref().unwrap_or("None.");
    let body = json!({
        "model": config.model,
        "messages": [
            {"role": "system", "content": format!("You summarize meeting transcripts. Correct obvious transcription errors using the provided instructions, but do not invent facts. Return Markdown only using this template:\n\n{SUMMARY_TEMPLATE}")},
            {"role": "user", "content": format!("Correction instructions:\n{corrections}\n\nTranscript:\n{transcript}")}
        ]
    });
    let client = Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()
        .context("could not create summary HTTP client")?;
    let request = apply_auth(client.post(&endpoint).json(&body), &config.auth)?;
    let response = request
        .send()
        .with_context(|| format!("summary request to {endpoint} failed"))?;
    let status = response.status();
    if !status.is_success() {
        bail!("summary request to {endpoint} failed with HTTP {status}")
    }
    let response: Value = response
        .json()
        .context("summary provider returned invalid JSON")?;
    response
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .context("summary provider response did not include choices[0].message.content")
}

fn apply_auth(mut request: RequestBuilder, auth: &AuthConfig) -> Result<RequestBuilder> {
    match auth {
        AuthConfig::None => {}
        AuthConfig::Bearer { token_env } => {
            request = request.header(
                AUTHORIZATION,
                format!("Bearer {}", environment_value(token_env)?),
            );
        }
        AuthConfig::Header {
            header_name,
            value_env,
        } => {
            let name = HeaderName::from_bytes(header_name.as_bytes())
                .context("configured authentication header name is invalid")?;
            let value = HeaderValue::from_str(&environment_value(value_env)?)
                .context("configured authentication header value is invalid")?;
            request = request.header(name, value);
        }
    }
    Ok(request)
}

fn environment_value(name: &str) -> Result<String> {
    let value = env::var(name)
        .with_context(|| format!("required environment variable {name} is not set"))?;
    if value.is_empty() {
        bail!("required environment variable {name} is empty")
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
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
            base_url: format!("http://{address}/v1"),
            chat_completions_path: "/chat/completions".into(),
            model: "DeepSeek-V4-Pro".into(),
            auth: AuthConfig::None,
            instructions: Some("Correct Acme to Acme Corp".into()),
        };

        let output = summarize(&transcript_path, Some(&config), false).unwrap();
        server.join().unwrap();
        assert_eq!(output.summary_path, directory.path().join(SUMMARY_FILE));
        assert_eq!(
            fs::read_to_string(output.summary_path).unwrap(),
            "## Summary\nAcme Corp met.\n"
        );
    }

    #[test]
    fn refuses_to_overwrite_summary_without_force() {
        let directory = tempfile::tempdir().unwrap();
        let transcript_path = directory.path().join("transcript.json");
        fs::write(&transcript_path, r#"{"schema_version":1,"text":"Meeting text","language":null,"duration_seconds":null,"segments":[],"provider":"test","model":"test","source_path":"test.wav","raw_response":null}"#).unwrap();
        fs::write(directory.path().join(SUMMARY_FILE), "existing summary").unwrap();
        let config = LlmConfig {
            base_url: "http://127.0.0.1:1".into(),
            chat_completions_path: "/chat/completions".into(),
            model: "test".into(),
            auth: AuthConfig::None,
            instructions: None,
        };

        let error = summarize(&transcript_path, Some(&config), false).unwrap_err();
        assert!(error.to_string().contains("pass --force"));
        assert_eq!(
            fs::read_to_string(directory.path().join(SUMMARY_FILE)).unwrap(),
            "existing summary"
        );
    }
}
