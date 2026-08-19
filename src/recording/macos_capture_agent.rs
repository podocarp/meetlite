//! Authenticated IPC between the CLI mixer and the signed capture companion.

use std::{
    fs::File,
    io::{ErrorKind, Read, Write},
    net::{TcpListener, TcpStream},
    path::PathBuf,
    process::Command,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use anyhow::{bail, Context, Result};

use super::{
    macos_system::SystemAudioCapture, AudioFrame, CaptureStatistics, SourceBuffer, SourceKind,
};

const AUTH_MAGIC: &[u8; 4] = b"MLCA";
const ACCEPT_MAGIC: &[u8; 4] = b"OKAY";
const HELLO_MAGIC: &[u8; 4] = b"MLCS";
const ERROR_MAGIC: &[u8; 4] = b"MLCE";
const FRAME_MAGIC: &[u8; 4] = b"MLCF";
const TOKEN_BYTES: usize = 32;
const MAX_FRAME_SAMPLES: usize = 48_000;
const START_TIMEOUT: Duration = Duration::from_secs(12);

pub struct AgentSystemAudioCapture {
    stream: TcpStream,
    received: Vec<u8>,
    sample_rate: u32,
    remote_origin: Option<(u64, Instant)>,
    dropped_frames: Arc<AtomicU64>,
}

impl AgentSystemAudioCapture {
    pub fn start() -> Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .context("could not create local listener for MeetliteCapture.app")?;
        listener
            .set_nonblocking(false)
            .context("could not configure local capture listener")?;
        let port = listener.local_addr()?.port();
        let token = random_token()?;
        launch_agent(port, &token)?;

        listener
            .set_nonblocking(true)
            .context("could not wait for MeetliteCapture.app")?;
        let deadline = Instant::now() + START_TIMEOUT;
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, address)) if address.ip().is_loopback() => break stream,
                Ok(_) => continue,
                Err(error) if error.kind() == ErrorKind::WouldBlock && Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => bail!(
                    "MeetliteCapture.app did not connect within {} seconds; ensure it is installed and grant Audio Capture permission when prompted",
                    START_TIMEOUT.as_secs()
                ),
                Err(error) => return Err(error).context("could not accept MeetliteCapture.app connection"),
            }
        };
        // Accepted sockets inherit the listener's nonblocking mode on macOS.
        // The authentication exchange needs bounded blocking reads instead.
        stream
            .set_nonblocking(false)
            .context("could not configure capture-agent authentication stream")?;
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .context("could not configure capture-agent authentication timeout")?;

        let mut auth = [0_u8; 4 + TOKEN_BYTES];
        stream
            .read_exact(&mut auth)
            .context("MeetliteCapture.app disconnected before authenticating")?;
        if auth[..4] != *AUTH_MAGIC || !constant_time_eq(&auth[4..], &token) {
            bail!("rejected an unauthenticated local capture-agent connection")
        }
        stream
            .write_all(ACCEPT_MAGIC)
            .context("could not acknowledge MeetliteCapture.app")?;

        let mut hello_magic = [0_u8; 4];
        stream
            .read_exact(&mut hello_magic)
            .context("MeetliteCapture.app did not start system-audio capture")?;
        if hello_magic == *ERROR_MAGIC {
            let mut length = [0_u8; 2];
            stream.read_exact(&mut length)?;
            let mut message = vec![0_u8; u16::from_le_bytes(length) as usize];
            stream.read_exact(&mut message)?;
            bail!(
                "MeetliteCapture.app could not start system-audio capture: {}",
                String::from_utf8_lossy(&message)
            )
        }
        if hello_magic != *HELLO_MAGIC {
            bail!("MeetliteCapture.app sent an invalid startup response")
        }
        let mut sample_rate_bytes = [0_u8; 4];
        stream.read_exact(&mut sample_rate_bytes)?;
        let sample_rate = u32::from_le_bytes(sample_rate_bytes);
        if sample_rate != super::SAMPLE_RATE {
            bail!("MeetliteCapture.app delivers {sample_rate} Hz; 48000 Hz is required")
        }
        stream
            .set_read_timeout(None)
            .context("could not configure capture-agent stream")?;
        stream
            .set_nonblocking(true)
            .context("could not configure capture-agent stream")?;

        Ok(Self {
            stream,
            received: Vec::with_capacity(MAX_FRAME_SAMPLES * 4),
            sample_rate,
            remote_origin: None,
            dropped_frames: Arc::new(AtomicU64::new(0)),
        })
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn drain_into(&mut self, output: &mut SourceBuffer) {
        let mut bytes = [0_u8; 16_384];
        loop {
            match self.stream.read(&mut bytes) {
                Ok(0) => break,
                Ok(count) => self.received.extend_from_slice(&bytes[..count]),
                Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                Err(_) => break,
            }
        }

        loop {
            if self.received.len() < 16 {
                return;
            }
            if self.received[..4] != *FRAME_MAGIC {
                self.received.clear();
                self.dropped_frames.fetch_add(1, Ordering::Relaxed);
                return;
            }
            let timestamp = u64::from_le_bytes(self.received[4..12].try_into().unwrap());
            let sample_count =
                u32::from_le_bytes(self.received[12..16].try_into().unwrap()) as usize;
            if sample_count == 0 || sample_count > MAX_FRAME_SAMPLES {
                self.received.clear();
                self.dropped_frames.fetch_add(1, Ordering::Relaxed);
                return;
            }
            let frame_bytes = 16 + sample_count * std::mem::size_of::<f32>();
            if self.received.len() < frame_bytes {
                return;
            }
            let (_, origin) = *self
                .remote_origin
                .get_or_insert_with(|| (timestamp, Instant::now()));
            let captured_at = origin
                .checked_add(Duration::from_nanos(
                    timestamp.saturating_sub(self.remote_origin.unwrap().0),
                ))
                .unwrap_or(origin);
            let samples = self.received[16..frame_bytes]
                .chunks_exact(4)
                .map(|bytes| f32::from_le_bytes(bytes.try_into().unwrap()))
                .collect();
            output.push(AudioFrame {
                source: SourceKind::System,
                captured_at,
                sample_rate: self.sample_rate,
                samples,
            });
            self.received.drain(..frame_bytes);
        }
    }

    pub fn stop(mut self, output: &mut SourceBuffer) -> CaptureStatistics {
        self.drain_into(output);
        drop(self.stream);
        CaptureStatistics {
            dropped_callback_frames: self.dropped_frames.load(Ordering::Relaxed),
            dropped_buffered_frames: output.dropped_frames(),
        }
    }
}

fn launch_agent(port: u16, token: &[u8; TOKEN_BYTES]) -> Result<()> {
    let agent = capture_agent_path()?;
    let status = Command::new("open")
        .arg("-n")
        .arg(&agent)
        .arg("--args")
        .arg("capture-agent")
        .arg("--port")
        .arg(port.to_string())
        .arg("--token")
        .arg(hex(token))
        .status()
        .context("could not invoke LaunchServices for MeetliteCapture.app")?;
    if !status.success() {
        bail!("LaunchServices could not start {}", agent.display())
    }
    Ok(())
}

fn capture_agent_path() -> Result<PathBuf> {
    let installed = crate::setup::installed_agent_path()?;
    if installed.join("Contents/MacOS/meetlite").is_file() {
        return Ok(installed);
    }

    let executable = std::env::current_exe().context("could not locate meetlite executable")?;
    let macos = executable
        .parent()
        .context("meetlite executable has no parent directory")?;
    let contents = macos
        .parent()
        .context("meetlite executable is not in an app bundle")?;
    let app = contents
        .parent()
        .context("meetlite executable is not in an app bundle")?;
    let agent = app.with_file_name("MeetliteCapture.app");
    if !agent.join("Contents/MacOS/meetlite").is_file() {
        bail!(
            "MeetliteCapture.app is unavailable at {} or {}; run `meetlite setup` or use the packaged Meetlite.app",
            installed.display(),
            agent.display()
        )
    }
    Ok(agent)
}

pub fn run_capture_agent(port: u16, token: String) -> Result<()> {
    let token = decode_token(&token)?;
    let address = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(10))
        .context("could not connect to the requesting Meetlite CLI")?;
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .context("could not configure capture-agent authentication timeout")?;
    stream
        .write_all(AUTH_MAGIC)
        .context("could not authenticate with the requesting Meetlite CLI")?;
    stream
        .write_all(&token)
        .context("could not authenticate with the requesting Meetlite CLI")?;
    let mut accepted = [0_u8; 4];
    stream
        .read_exact(&mut accepted)
        .context("the requesting Meetlite CLI did not acknowledge capture-agent authentication")?;
    if accepted != *ACCEPT_MAGIC {
        bail!("the requesting Meetlite CLI rejected capture-agent authentication")
    }

    let started = Instant::now();
    let mut capture = match SystemAudioCapture::start() {
        Ok(capture) => capture,
        Err(error) => {
            let message = error.to_string();
            let message = message.as_bytes();
            let message = &message[..message.len().min(u16::MAX as usize)];
            let _ = stream.write_all(ERROR_MAGIC);
            let _ = stream.write_all(&(message.len() as u16).to_le_bytes());
            let _ = stream.write_all(message);
            return Err(error);
        }
    };
    let sample_rate = capture.sample_rate();
    stream.write_all(HELLO_MAGIC)?;
    stream.write_all(&sample_rate.to_le_bytes())?;
    stream.set_read_timeout(Some(Duration::from_millis(1)))?;
    loop {
        let mut probe = [0_u8; 1];
        match stream.peek(&mut probe) {
            Ok(0) => break,
            Ok(_) => bail!("the requesting Meetlite CLI sent unexpected capture-agent data"),
            Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(error) => return Err(error).context("could not monitor Meetlite CLI connection"),
        }
        let mut frames = SourceBuffer::new(SourceKind::System);
        capture.drain_into(&mut frames);
        while let Some(frame) = frames.pop_frame() {
            send_frame(&mut stream, &frame, started)?;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    let mut discarded = SourceBuffer::new(SourceKind::System);
    let _ = capture.stop(&mut discarded);
    Ok(())
}

fn send_frame(stream: &mut TcpStream, frame: &AudioFrame, started: Instant) -> Result<()> {
    let timestamp = frame
        .captured_at
        .checked_duration_since(started)
        .unwrap_or_default()
        .as_nanos() as u64;
    stream.write_all(FRAME_MAGIC)?;
    stream.write_all(&timestamp.to_le_bytes())?;
    stream.write_all(&(frame.samples.len() as u32).to_le_bytes())?;
    for sample in &frame.samples {
        stream.write_all(&sample.to_le_bytes())?;
    }
    Ok(())
}

fn random_token() -> Result<[u8; TOKEN_BYTES]> {
    let mut token = [0_u8; TOKEN_BYTES];
    File::open("/dev/urandom")
        .context("could not open the system random source")?
        .read_exact(&mut token)
        .context("could not generate a capture-agent authentication token")?;
    Ok(token)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn decode_token(value: &str) -> Result<[u8; TOKEN_BYTES]> {
    if value.len() != TOKEN_BYTES * 2 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("capture-agent token is malformed")
    }
    let mut token = [0_u8; TOKEN_BYTES];
    for (index, byte) in token.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)?;
    }
    Ok(token)
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .fold(0_u8, |difference, (left, right)| {
                difference | (left ^ right)
            })
            == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_agent_tokens_require_exact_hex_and_match_in_constant_time() {
        let token = [0x5a; TOKEN_BYTES];
        assert_eq!(decode_token(&hex(&token)).unwrap(), token);
        assert!(decode_token("not-a-token").is_err());
        assert!(constant_time_eq(&token, &token));
        assert!(!constant_time_eq(&token, &[0x5a; TOKEN_BYTES - 1]));
        assert!(!constant_time_eq(&token, &[0x00; TOKEN_BYTES]));
    }
}
