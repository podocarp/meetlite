use std::{env, fs, path::Path, time::Duration};

use anyhow::{bail, Context, Result};
use reqwest::{
    blocking::{multipart, Client},
    header::{HeaderName, HeaderValue, AUTHORIZATION},
};
use serde::Deserialize;
use serde_json::Value;

use super::{Transcript, TranscriptSegment};
use crate::config::{AuthConfig, SttConfig};

const MAX_UPLOAD_BYTES: u64 = 100 * 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

pub(crate) fn transcribe(input: &Path, config: &SttConfig) -> Result<Transcript> {
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

    let endpoint = format!(
        "{}{}",
        config.base_url.trim_end_matches('/'),
        config.transcription_path
    );
    let mut request = Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()
        .context("could not create transcription HTTP client")?
        .post(&endpoint)
        .multipart(form);
    request = apply_auth(request, &config.auth)?;
    let response = request
        .send()
        .with_context(|| format!("transcription request to {endpoint} failed"))?;
    let status = response.status();
    if !status.is_success() {
        bail!("transcription request to {endpoint} failed with HTTP {status}")
    }
    let raw_response: Value = response
        .json()
        .context("transcription provider returned invalid JSON")?;
    normalize(raw_response, config, input)
}

fn apply_auth(
    mut request: reqwest::blocking::RequestBuilder,
    auth: &AuthConfig,
) -> Result<reqwest::blocking::RequestBuilder> {
    match auth {
        AuthConfig::None => {}
        AuthConfig::Bearer { token_env } => {
            let token = required_environment_value(token_env)?;
            request = request.header(AUTHORIZATION, format!("Bearer {token}"));
        }
        AuthConfig::Header {
            header_name,
            value_env,
        } => {
            let name = HeaderName::from_bytes(header_name.as_bytes())
                .context("configured authentication header name is invalid")?;
            let value = HeaderValue::from_str(&required_environment_value(value_env)?)
                .context("configured authentication header value is invalid")?;
            request = request.header(name, value);
        }
    }
    Ok(request)
}

fn required_environment_value(name: &str) -> Result<String> {
    let value = env::var(name)
        .with_context(|| format!("required environment variable {name} is not set"))?;
    if value.is_empty() {
        bail!("required environment variable {name} is empty")
    }
    Ok(value)
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
            base_url,
            transcription_path: "/audio/transcriptions".into(),
            model: "whisper-test".into(),
            language: Some("en".into()),
            response_format: "verbose_json".into(),
            auth: AuthConfig::Bearer {
                token_env: "MEETLITE_TEST_TOKEN".into(),
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
        env::set_var("MEETLITE_TEST_TOKEN", "test-token");
        let transcript = crate::transcription::transcribe_file(
            &input,
            Some(temporary.path()),
            &config(format!("http://{address}/v1")),
        )
        .unwrap();
        env::remove_var("MEETLITE_TEST_TOKEN");
        server.join().unwrap();

        assert_eq!(transcript.text, "hello world");
        assert_eq!(transcript.language.as_deref(), Some("en"));
        assert_eq!(transcript.segments.len(), 1);
        assert_eq!(transcript.segments[0].end_seconds, 1.25);
        assert!(temporary.path().join("transcript.json").is_file());
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
