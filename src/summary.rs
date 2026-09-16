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
    config::LlmConfig, credentials::Credentials, live_control::LiveControl, output::Output,
    transcription::Transcript,
};

const SUMMARY_FILE: &str = "summary.md";
const MAX_ENDPOINT_DISPLAY_CHARS: usize = 512;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
const SUMMARY_TEMPLATE: &str = "## Summary\n\n## Decisions\n\n## Action Items\n\n## Open Questions";
const EMPTY_PARTIAL_SUMMARY: &str = "## Summary\n\nNo completed transcription text was available before processing was stopped.\n\n## Decisions\n\nUnavailable.\n\n## Action Items\n\nUnavailable.\n\n## Open Questions\n\nUnavailable.\n";

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
    summarize_until(input, config, force, output, None, || false)?
        .context("summary was cancelled without a cancellation request")
}

pub(crate) fn write_empty_partial(
    input: &Path,
    force: bool,
    output: Output,
    control: &LiveControl,
) -> Result<Option<SummaryOutput>> {
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
    commit_summary(
        &summary_path,
        "",
        EMPTY_PARTIAL_SUMMARY.to_owned(),
        output,
        false,
        Some(control),
        || control.summary_stopped(),
    )
}

pub(crate) fn summarize_until(
    input: &Path,
    config: Option<&LlmConfig>,
    force: bool,
    output: Output,
    control: Option<&LiveControl>,
    stopped: impl Fn() -> bool,
) -> Result<Option<SummaryOutput>> {
    if stopped() {
        return Ok(None);
    }
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
    let Some(summary) = request_summary(
        &transcript.text,
        config,
        &credentials,
        output,
        control,
        &stopped,
    )?
    else {
        return Ok(None);
    };
    if summary.trim().is_empty() {
        bail!("LLM response did not include a summary")
    }
    let finish_line = !output.is_json() && !summary.ends_with('\n');
    let summary = format!("{}\n", summary.trim_end());
    commit_summary(
        &summary_path,
        &config.model,
        summary,
        output,
        finish_line,
        control,
        stopped,
    )
}

fn request_summary(
    transcript: &str,
    config: &LlmConfig,
    credentials: &Credentials,
    output: Output,
    control: Option<&LiveControl>,
    stopped: &impl Fn() -> bool,
) -> Result<Option<String>> {
    let endpoint = format!(
        "{}{}",
        config.base_url.trim_end_matches('/'),
        config.chat_completions_path
    );
    let endpoint_display = sanitize_endpoint(&endpoint);
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
    if stopped() {
        return Ok(None);
    }
    let response = request.send();
    if stopped() {
        return Ok(None);
    }
    let response = match response {
        Ok(response) => response,
        Err(error) => bail!(
            "summary request to {endpoint_display} {}",
            request_failure(&error)
        ),
    };
    let status = response.status();
    if !status.is_success() {
        bail!("summary request to {endpoint_display} failed with HTTP {status}")
    }
    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    if content_type.starts_with("text/event-stream") {
        read_stream(response, output, control, stopped)
    } else {
        let response = response.json();
        if stopped() {
            return Ok(None);
        }
        let response: Value = match response {
            Ok(response) => response,
            Err(_) => bail!("summary provider returned invalid JSON"),
        };
        let summary = response
            .pointer("/choices/0/message/content")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .context("summary provider response did not include choices[0].message.content")?;
        if !commit_while_running(control, stopped, || emit_delta(output, &summary))? {
            return Ok(None);
        }
        Ok(Some(summary))
    }
}

fn request_failure(error: &reqwest::Error) -> &'static str {
    if error.is_timeout() {
        "timed out"
    } else if error.is_connect() {
        "could not connect"
    } else {
        "failed"
    }
}

fn sanitize_endpoint(endpoint: &str) -> String {
    let Ok(url) = reqwest::Url::parse(endpoint) else {
        return "configured endpoint".to_owned();
    };
    let endpoint = format!("{}{}", url.origin().ascii_serialization(), url.path());
    let mut characters = endpoint.chars();
    let bounded: String = characters
        .by_ref()
        .take(MAX_ENDPOINT_DISPLAY_CHARS)
        .collect();
    if characters.next().is_some() {
        format!("{bounded}…")
    } else {
        bounded
    }
}

fn read_stream(
    response: Response,
    output: Output,
    control: Option<&LiveControl>,
    stopped: &impl Fn() -> bool,
) -> Result<Option<String>> {
    let mut reader = BufReader::new(response);
    let mut summary = String::new();
    let mut data = Vec::new();
    loop {
        if stopped() {
            return Ok(None);
        }
        let mut line = String::new();
        let bytes = reader.read_line(&mut line);
        if stopped() {
            return Ok(None);
        }
        let bytes = bytes.context("could not read summary stream")?;
        if bytes == 0 {
            if !data.is_empty() {
                let Some(done) =
                    process_event_until(&data.join("\n"), &mut summary, output, control, stopped)?
                else {
                    return Ok(None);
                };
                if done {
                    break;
                }
            }
            break;
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            if !data.is_empty() {
                let Some(done) =
                    process_event_until(&data.join("\n"), &mut summary, output, control, stopped)?
                else {
                    return Ok(None);
                };
                if done {
                    break;
                }
            }
            data.clear();
        } else if let Some(value) = line.strip_prefix("data:") {
            data.push(value.strip_prefix(' ').unwrap_or(value).to_owned());
        }
    }
    Ok(Some(summary))
}

fn process_event_until(
    data: &str,
    summary: &mut String,
    output: Output,
    control: Option<&LiveControl>,
    stopped: &impl Fn() -> bool,
) -> Result<Option<bool>> {
    if data == "[DONE]" {
        return Ok(Some(true));
    }
    let event: Value =
        serde_json::from_str(data).context("summary provider returned invalid SSE data")?;
    if let Some(delta) = event
        .pointer("/choices/0/delta/content")
        .and_then(Value::as_str)
        .filter(|delta| !delta.is_empty())
    {
        if !commit_while_running(control, stopped, || {
            emit_delta(output, delta)?;
            summary.push_str(delta);
            Ok(())
        })? {
            return Ok(None);
        }
    }
    Ok(Some(false))
}

fn commit_while_running(
    control: Option<&LiveControl>,
    stopped: &impl Fn() -> bool,
    action: impl FnOnce() -> Result<()>,
) -> Result<bool> {
    let commit = || {
        if stopped() {
            return Ok(false);
        }
        action()?;
        Ok(true)
    };
    match control {
        Some(control) => control.at_commit_boundary(commit),
        None => commit(),
    }
}

fn commit_summary(
    summary_path: &Path,
    model: &str,
    summary: String,
    output: Output,
    finish_line: bool,
    control: Option<&LiveControl>,
    stopped: impl Fn() -> bool,
) -> Result<Option<SummaryOutput>> {
    let temporary = summary_path.with_extension("md.tmp");
    let result = SummaryOutput {
        summary_path: summary_path.to_path_buf(),
        model: model.to_owned(),
        summary,
    };
    let committed = commit_while_running(control, &stopped, || {
        fs::write(&temporary, &result.summary)
            .with_context(|| format!("could not write {}", temporary.display()))?;
        fs::rename(&temporary, summary_path)
            .with_context(|| format!("could not write {}", summary_path.display()))?;
        if finish_line {
            output.line("")?;
        }
        emit_completed(output, &result)
    })?;
    if !committed {
        return Ok(None);
    }
    Ok(Some(result))
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
        sync::{Arc, Mutex},
        thread,
    };

    #[test]
    fn observed_cancellation_blocks_later_summary_side_effects() {
        let control = LiveControl::testing(2);
        let effects = Arc::new(Mutex::new(Vec::new()));
        let (started_sender, started_receiver) = crossbeam_channel::bounded(1);
        let (release_sender, release_receiver) = crossbeam_channel::bounded(1);
        let worker_control = control.clone();
        let worker_effects = Arc::clone(&effects);
        let worker = thread::spawn(move || {
            commit_while_running(
                Some(&worker_control),
                &|| worker_control.summary_stopped(),
                || {
                    started_sender.send(()).unwrap();
                    release_receiver.recv().unwrap();
                    worker_effects.lock().unwrap().push("delta");
                    Ok(())
                },
            )
            .unwrap()
        });
        started_receiver.recv().unwrap();
        let interrupt_control = control.clone();
        let interrupt = thread::spawn(move || interrupt_control.test_interrupt());
        release_sender.send(()).unwrap();

        assert!(worker.join().unwrap());
        interrupt.join().unwrap();
        assert!(control.summary_stopped());
        assert!(
            !commit_while_running(Some(&control), &|| control.summary_stopped(), || {
                effects.lock().unwrap().push("late");
                Ok(())
            })
            .unwrap()
        );
        assert_eq!(*effects.lock().unwrap(), vec!["delta"]);
    }

    #[test]
    fn cancellation_linearizes_before_summary_commit() {
        let directory = tempfile::tempdir().unwrap();
        let transcript_path = directory.path().join("transcript.json");
        let control = LiveControl::testing(2);
        control.test_interrupt();

        let result =
            write_empty_partial(&transcript_path, false, Output::new(false), &control).unwrap();

        assert!(result.is_none());
        assert!(!directory.path().join(SUMMARY_FILE).exists());
    }

    #[test]
    fn empty_cancelled_transcript_commits_truthful_summary_artifact() {
        let directory = tempfile::tempdir().unwrap();
        let transcript_path = directory.path().join("transcript.json");
        let control = LiveControl::testing(2);

        let result = write_empty_partial(&transcript_path, false, Output::new(false), &control)
            .unwrap()
            .unwrap();

        assert_eq!(result.model, "");
        assert_eq!(result.summary, EMPTY_PARTIAL_SUMMARY);
        assert_eq!(
            fs::read_to_string(directory.path().join(SUMMARY_FILE)).unwrap(),
            EMPTY_PARTIAL_SUMMARY
        );
    }

    #[test]
    fn cancellation_before_summary_skips_provider_and_output() {
        let directory = tempfile::tempdir().unwrap();
        let transcript_path = directory.path().join("transcript.json");

        let result = summarize_until(
            &transcript_path,
            None,
            false,
            Output::new(false),
            None,
            || true,
        )
        .unwrap();

        assert!(result.is_none());
        assert!(!directory.path().join(SUMMARY_FILE).exists());
    }

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
    fn provider_http_error_excludes_body_auth_and_url_secrets() {
        const BODY_SECRET: &str = "BODY_SECRET_SENTINEL";
        const USERINFO_SECRET: &str = "USERINFO_SECRET_SENTINEL";
        const QUERY_SECRET: &str = "QUERY_SECRET_SENTINEL";
        const AUTH_SECRET: &str = "AUTH_SECRET_SENTINEL";

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
            let body = format!(r#"{{"error":"{BODY_SECRET}"}}"#);
            write!(stream, "HTTP/1.1 429 Too Many Requests\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
        });
        let config = LlmConfig {
            api_style: crate::config::ApiStyle::OpenAiCompatible,
            base_url: format!("http://user:{USERINFO_SECRET}@{address}/v1"),
            chat_completions_path: format!("/chat/completions?api_key={QUERY_SECRET}"),
            model: "test".into(),
            auth: AuthConfig::BearerPlain {
                token: AUTH_SECRET.into(),
            },
            instructions: None,
        };
        let credentials = Credentials::for_llm(&config.auth).unwrap();

        let error = request_summary(
            "Meeting text",
            &config,
            &credentials,
            Output::new(false),
            None,
            &|| false,
        )
        .unwrap_err();
        server.join().unwrap();

        let message = error.to_string();
        assert!(message.contains("429 Too Many Requests"));
        assert!(message.contains(&format!("http://{address}/v1/chat/completions")));
        for secret in [BODY_SECRET, USERINFO_SECRET, QUERY_SECRET, AUTH_SECRET] {
            assert!(!message.contains(secret));
        }
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
