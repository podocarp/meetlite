use std::{fs, path::Path, time::Duration};

use anyhow::{bail, Context, Result};
use reqwest::blocking::{multipart, Client};
use serde::Deserialize;
use serde_json::Value;

use super::{Transcript, TranscriptSegment};
use crate::{config::SttConfig, credentials::Credentials};

const MAX_UPLOAD_BYTES: u64 = 100 * 1024 * 1024;
const MAX_ENDPOINT_DISPLAY_CHARS: usize = 512;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

#[cfg(test)]
pub(crate) fn transcribe(
    input: &Path,
    config: &SttConfig,
    credentials: &Credentials,
    prompt: Option<&str>,
) -> Result<Transcript> {
    transcribe_until(input, config, credentials, prompt, &|| false)?
        .context("transcription was cancelled without a cancellation request")
}

pub(crate) fn transcribe_until(
    input: &Path,
    config: &SttConfig,
    credentials: &Credentials,
    prompt: Option<&str>,
    stopped: &impl Fn() -> bool,
) -> Result<Option<Transcript>> {
    if stopped() {
        return Ok(None);
    }
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
    if stopped() {
        return Ok(None);
    }
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
    let endpoint_display = sanitize_endpoint(&endpoint);
    let client = Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()
        .context("could not create transcription HTTP client")?;
    if stopped() {
        return Ok(None);
    }
    let request = credentials.apply(client.post(&endpoint).multipart(form));
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
            "transcription request to {endpoint_display} {}",
            request_failure(&error)
        ),
    };
    let status = response.status();
    if !status.is_success() {
        bail!("transcription request to {endpoint_display} failed with HTTP {status}")
    }
    let raw_response = response.json();
    if stopped() {
        return Ok(None);
    }
    let raw_response: Value = match raw_response {
        Ok(response) => response,
        Err(_) => bail!("transcription provider returned invalid JSON"),
    };
    normalize(raw_response, config, input).map(Some)
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

fn normalize(raw_response: Value, config: &SttConfig, input: &Path) -> Result<Transcript> {
    let response: ProviderTranscript = serde_json::from_value(raw_response)
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
            speaker: None,
        })
        .collect();
    Ok(Transcript {
        schema_version: 1,
        text: response.text,
        language: response.language,
        duration_seconds: response.duration,
        segments,
        provider: sanitize_endpoint(&config.base_url),
        model: config.model.clone(),
        source_path: input.display().to_string(),
        raw_response: Value::Null,
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
    fn cancellation_before_request_skips_input_and_provider_work() {
        let config = config("http://127.0.0.1:1/v1".into());
        let credentials = Credentials::for_stt(&config.auth).unwrap();

        let transcript = transcribe_until(
            Path::new("missing.wav"),
            &config,
            &credentials,
            None,
            &|| true,
        )
        .unwrap();

        assert!(transcript.is_none());
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
            write!(stream, "HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
        });

        let temporary = tempfile::tempdir().unwrap();
        let input = temporary.path().join("sample.wav");
        fs::write(&input, b"RIFF test fixture").unwrap();
        let mut config = config(format!("http://user:{USERINFO_SECRET}@{address}/v1"));
        config.transcription_path = format!("/audio/transcriptions?api_key={QUERY_SECRET}");
        config.auth = AuthConfig::BearerPlain {
            token: AUTH_SECRET.into(),
        };
        let credentials = Credentials::for_stt(&config.auth).unwrap();
        let error = transcribe(&input, &config, &credentials, None).unwrap_err();
        server.join().unwrap();

        let message = error.to_string();
        assert!(message.contains("401 Unauthorized"));
        assert!(message.contains(&format!("http://{address}/v1/audio/transcriptions")));
        for secret in [BODY_SECRET, USERINFO_SECRET, QUERY_SECRET, AUTH_SECRET] {
            assert!(!message.contains(secret));
        }
    }

    #[test]
    fn normalized_transcript_excludes_raw_response_and_url_secrets() {
        const BODY_SECRET: &str = "BODY_SECRET_SENTINEL";
        const USERINFO_SECRET: &str = "USERINFO_SECRET_SENTINEL";
        const QUERY_SECRET: &str = "QUERY_SECRET_SENTINEL";

        let transcript = normalize(
            serde_json::json!({
                "text": "hello",
                "provider_metadata": BODY_SECRET,
            }),
            &config(format!(
                "https://user:{USERINFO_SECRET}@provider.test/v1?api_key={QUERY_SECRET}"
            )),
            Path::new("audio.wav"),
        )
        .unwrap();
        let serialized = serde_json::to_string(&transcript).unwrap();

        assert_eq!(transcript.provider, "https://provider.test/v1");
        assert_eq!(transcript.raw_response, Value::Null);
        for secret in [BODY_SECRET, USERINFO_SECRET, QUERY_SECRET] {
            assert!(!serialized.contains(secret));
        }
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
