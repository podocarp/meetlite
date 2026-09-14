use std::{fs, path::Path, time::Duration};

use anyhow::{bail, Context, Result};
use reqwest::blocking::{multipart, Client};
use serde::Deserialize;
use serde_json::Value;

use super::{Transcript, TranscriptSegment};
use crate::{config::SttConfig, credentials::Credentials};

const MAX_UPLOAD_BYTES: u64 = 100 * 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

pub(crate) fn transcribe(
    input: &Path,
    config: &SttConfig,
    credentials: &Credentials,
    prompt: Option<&str>,
) -> Result<Transcript> {
    let metadata = fs::metadata(input)
        .with_context(|| format!("could not read input audio file {}", input.display()))?;
    if !metadata.is_file() {
        bail!("input audio path {} is not a file", input.display())
    }
    if metadata.len() > MAX_UPLOAD_BYTES {
        bail!(
            "input audio file is {} bytes, exceeding the {} byte upload limit",
            metadata.len(),
            MAX_UPLOAD_BYTES
        )
    }
    let filename = input
        .file_name()
        .and_then(|name| name.to_str())
        .context("input audio file name must be valid UTF-8")?;
    let audio = fs::read(input).with_context(|| format!("could not read {}", input.display()))?;
    let part = multipart::Part::bytes(audio)
        .file_name(filename.to_owned())
        .mime_str("application/octet-stream")?;
    let mut form = multipart::Form::new()
        .part("file", part)
        .text("model", config.model.clone())
        .text("response_format", config.response_format.clone());
    if let Some(language) = config.language.as_deref() {
        form = form.text("language", language.to_owned());
    }
    if let Some(prompt) = prompt.filter(|prompt| !prompt.trim().is_empty()) {
        form = form.text("prompt", prompt.to_owned());
    }

    let endpoint = format!(
        "{}{}",
        config.base_url.trim_end_matches('/'),
        config.transcription_path
    );
    let request = Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()
        .context("could not create transcription HTTP client")?
        .post(&endpoint)
        .multipart(form);
    let request = credentials.apply(request);
    let response = request
        .send()
        .with_context(|| format!("transcription request to {endpoint} failed"))?;
    let status = response.status();
    if !status.is_success() {
        let body = response
            .text()
            .context("could not read transcription provider error response")?;
        let detail = truncate_error_body(body.trim(), 4096);
        if detail.is_empty() {
            bail!("transcription request to {endpoint} failed with HTTP {status}")
        }
        bail!("transcription request to {endpoint} failed with HTTP {status}: {detail}")
    }
    let raw_response: Value = response
        .json()
        .context("transcription provider returned invalid JSON")?;
    normalize(raw_response, config, input)
}

fn truncate_error_body(body: &str, max_chars: usize) -> String {
    let mut characters = body.chars();
    let truncated: String = characters.by_ref().take(max_chars).collect();
    if characters.next().is_some() {
        format!("{truncated}…")
    } else {
        truncated
    }
}

fn normalize(raw_response: Value, config: &SttConfig, input: &Path) -> Result<Transcript> {
    let response: ProviderTranscript = serde_json::from_value(raw_response.clone())
        .context("transcription provider response has an invalid transcript shape")?;
    if response.text.trim().is_empty() {
        bail!("transcription provider response did not include transcript text")
    }
    let segments = response
        .segments
        .unwrap_or_default()
        .into_iter()
        .map(|segment| TranscriptSegment {
            start_seconds: segment.start,
            end_seconds: segment.end,
            text: segment.text,
        })
        .collect();
    Ok(Transcript {
        schema_version: 1,
        text: response.text,
        language: response.language,
        duration_seconds: response.duration,
        segments,
        provider: config.base_url.clone(),
        model: config.model.clone(),
        source_path: input.display().to_string(),
        raw_response,
    })
}

#[derive(Deserialize)]
struct ProviderTranscript {
    text: String,
    language: Option<String>,
    duration: Option<f64>,
    segments: Option<Vec<ProviderSegment>>,
}

#[derive(Deserialize)]
struct ProviderSegment {
    start: f64,
    end: f64,
    text: String,
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

    fn config(base_url: String) -> SttConfig {
        SttConfig {
            api_style: crate::config::ApiStyle::OpenAiCompatible,
            base_url,
            transcription_path: "/audio/transcriptions".into(),
            model: "whisper-test".into(),
            language: Some("en".into()),
            prompt: Some("Meetlite, Kubernetes".into()),
            response_format: "verbose_json".into(),
            auth: AuthConfig::BearerPlain {
                token: "test-token".into(),
            },
        }
    }

    #[test]
    fn uploads_openai_multipart_request_and_normalizes_response() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut buffer = [0; 4096];
            let body_length = loop {
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
                    break headers_end + 4 + content_length;
                }
            };
            request.truncate(body_length);
            let request = String::from_utf8_lossy(&request).to_ascii_lowercase();
            assert!(request.starts_with("post /v1/audio/transcriptions http/1.1"));
            assert!(request.contains("authorization: bearer test-token"));
            assert!(request.contains("name=\"model\""));
            assert!(request.contains("whisper-test"));
            assert!(request.contains("name=\"language\""));
            assert!(request.contains("name=\"prompt\""));
            assert!(request.contains("meetlite, kubernetes"));
            assert!(request.contains("name=\"file\"; filename=\"sample.wav\""));
            let body = r#"{"text":"hello world","language":"en","duration":1.25,"segments":[{"start":0.0,"end":1.25,"text":"hello world"}]}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        });

        let temporary = tempfile::tempdir().unwrap();
        let input = temporary.path().join("sample.wav");
        fs::write(&input, b"RIFF test fixture").unwrap();
        let output = crate::transcription::transcribe_file(
            &input,
            Some(temporary.path()),
            &config(format!("http://{address}/v1")),
            false,
            crate::output::Output::new(false),
        )
        .unwrap();
        server.join().unwrap();

        let transcript: Transcript =
            serde_json::from_slice(&fs::read(&output.transcript_path).unwrap()).unwrap();
        assert_eq!(transcript.text, "hello world");
        assert_eq!(transcript.language.as_deref(), Some("en"));
        assert_eq!(transcript.segments.len(), 1);
        assert_eq!(transcript.segments[0].end_seconds, 1.25);
        assert!(output.transcript_path.is_file());
    }

    #[test]
    fn includes_provider_error_body_in_http_error() {
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
            let body = r#"{"error":{"message":"response format is not implemented"}}"#;
            write!(stream, "HTTP/1.1 501 Not Implemented\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
        });

        let temporary = tempfile::tempdir().unwrap();
        let input = temporary.path().join("sample.wav");
        fs::write(&input, b"RIFF test fixture").unwrap();
        let config = config(format!("http://{address}/v1"));
        let credentials = Credentials::for_stt(&config.auth).unwrap();
        let error = transcribe(&input, &config, &credentials, None).unwrap_err();
        server.join().unwrap();

        let message = error.to_string();
        assert!(message.contains("501 Not Implemented"));
        assert!(message.contains("response format is not implemented"));
    }

    #[test]
    fn rejects_response_without_text() {
        let error = normalize(
            serde_json::json!({"language": "en"}),
            &config("http://127.0.0.1".into()),
            Path::new("audio.wav"),
        )
        .unwrap_err();
        assert!(error.to_string().contains("invalid transcript shape"));
    }
}
