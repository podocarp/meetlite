use std::{
    env, fs,
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

const CONFIG_ENV: &str = "MEETLITE_CONFIG";

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub recording: RecordingConfig,
    pub stt: Option<SttConfig>,
    pub llm: Option<LlmConfig>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RecordingConfig {
    #[serde(default = "default_sample_rate")]
    pub sample_rate: u32,
    #[serde(default = "default_microphone_gain")]
    pub microphone_gain: f32,
    #[serde(default = "default_system_gain")]
    pub system_gain: f32,
    pub microphone_device: Option<String>,
    pub system_device: Option<String>,
}

impl Default for RecordingConfig {
    fn default() -> Self {
        Self {
            sample_rate: default_sample_rate(),
            microphone_gain: default_microphone_gain(),
            system_gain: default_system_gain(),
            microphone_device: None,
            system_device: None,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SttConfig {
    pub base_url: String,
    pub model: String,
    pub language: Option<String>,
    #[serde(default = "default_response_format")]
    pub response_format: String,
    pub auth: AuthConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LlmConfig {
    pub base_url: String,
    pub model: String,
    pub auth: AuthConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AuthConfig {
    None,
    Bearer {
        token_env: String,
    },
    Header {
        header_name: String,
        value_env: String,
    },
}

impl Config {
    pub fn path(override_path: Option<&Path>) -> Result<PathBuf> {
        if let Some(path) = override_path {
            return Ok(path.to_path_buf());
        }

        if let Some(path) = env::var_os(CONFIG_ENV) {
            return Ok(PathBuf::from(path));
        }

        let config_home = env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
            .context("could not determine a config directory; set MEETLITE_CONFIG")?;

        Ok(config_home.join("meetlite").join("config.json"))
    }

    pub fn initialize(override_path: Option<&Path>) -> Result<PathBuf> {
        let path = Self::path(override_path)?;
        let parent = path
            .parent()
            .context("configuration path must include a parent directory")?;
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "could not create configuration directory {}",
                parent.display()
            )
        })?;
        set_private_directory_permissions(parent)?;

        let contents = serde_json::to_string_pretty(&Self {
            recording: RecordingConfig::default(),
            stt: None,
            llm: None,
        })? + "\n";
        let mut file = new_private_file(&path)?;
        file.write_all(contents.as_bytes())
            .with_context(|| format!("could not write configuration file {}", path.display()))?;

        Ok(path)
    }

    pub fn load(override_path: Option<&Path>) -> Result<Self> {
        let path = Self::path(override_path)?;
        let contents = fs::read_to_string(&path).with_context(|| {
            format!(
                "could not read configuration {}; run `meetlite config init` to create it",
                path.display()
            )
        })?;
        let config: Self = serde_json::from_str(&contents)
            .with_context(|| format!("configuration {} is not valid JSON", path.display()))?;
        config.validate()?;
        Ok(config)
    }

    pub fn load_if_present(override_path: Option<&Path>) -> Result<Option<Self>> {
        let path = Self::path(override_path)?;
        if !path.exists() {
            return Ok(None);
        }
        Self::load(Some(&path)).map(Some)
    }

    pub fn stt(&self) -> Result<&SttConfig> {
        self.stt.as_ref().context(
            "no STT provider is configured; add an `stt` section to the Meetlite configuration",
        )
    }

    fn validate(&self) -> Result<()> {
        if self.recording.sample_rate != 48_000 {
            bail!("recording.sample_rate must be 48000 for the initial recorder")
        }
        validate_gain("recording.microphone_gain", self.recording.microphone_gain)?;
        validate_gain("recording.system_gain", self.recording.system_gain)?;

        if let Some(stt) = &self.stt {
            validate_provider("stt", &stt.base_url, &stt.model, &stt.auth)?;
            if stt.response_format.trim().is_empty() {
                bail!("stt.response_format must not be empty")
            }
        }
        if let Some(llm) = &self.llm {
            validate_provider("llm", &llm.base_url, &llm.model, &llm.auth)?;
        }

        Ok(())
    }
}

fn default_sample_rate() -> u32 {
    48_000
}

fn default_microphone_gain() -> f32 {
    1.0
}

fn default_system_gain() -> f32 {
    0.8
}

fn default_response_format() -> String {
    "verbose_json".into()
}

fn validate_gain(name: &str, gain: f32) -> Result<()> {
    if !gain.is_finite() || gain < 0.0 {
        bail!("{name} must be a finite, non-negative number")
    }
    Ok(())
}

fn validate_provider(name: &str, base_url: &str, model: &str, auth: &AuthConfig) -> Result<()> {
    if !(base_url.starts_with("https://") || base_url.starts_with("http://")) {
        bail!("{name}.base_url must start with http:// or https://")
    }
    if model.trim().is_empty() {
        bail!("{name}.model must not be empty")
    }

    match auth {
        AuthConfig::None => {}
        AuthConfig::Bearer { token_env } => {
            validate_env_name(&format!("{name}.auth.token_env"), token_env)?
        }
        AuthConfig::Header {
            header_name,
            value_env,
        } => {
            if header_name.trim().is_empty()
                || !header_name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
            {
                bail!("{name}.auth.header_name must be a valid HTTP header name")
            }
            validate_env_name(&format!("{name}.auth.value_env"), value_env)?;
        }
    }

    Ok(())
}

fn validate_env_name(name: &str, value: &str) -> Result<()> {
    let mut chars = value.bytes();
    let Some(first) = chars.next() else {
        bail!("{name} must not be empty")
    };
    if !(first.is_ascii_alphabetic() || first == b'_')
        || !chars.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        bail!("{name} must be an environment variable name")
    }
    Ok(())
}

#[cfg(unix)]
fn set_private_directory_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .with_context(|| format!("could not restrict permissions on {}", path.display()))
}

#[cfg(not(unix))]
fn set_private_directory_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn new_private_file(path: &Path) -> Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;

    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .with_context(|| {
            format!(
                "refusing to overwrite existing configuration {}",
                path.display()
            )
        })
}

#[cfg(not(unix))]
fn new_private_file(path: &Path) -> Result<fs::File> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| {
            format!(
                "refusing to overwrite existing configuration {}",
                path.display()
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_with_stt(auth: AuthConfig) -> Config {
        Config {
            recording: RecordingConfig::default(),
            stt: Some(SttConfig {
                base_url: "https://stt.example.test/v1".into(),
                model: "whisper-test".into(),
                language: None,
                response_format: default_response_format(),
                auth,
            }),
            llm: None,
        }
    }

    #[test]
    fn accepts_a_valid_bearer_provider() {
        config_with_stt(AuthConfig::Bearer {
            token_env: "MEETLITE_STT_API_KEY".into(),
        })
        .validate()
        .unwrap();
    }

    #[test]
    fn rejects_invalid_recording_rate() {
        let mut config = config_with_stt(AuthConfig::None);
        config.recording.sample_rate = 44_100;

        assert!(config.validate().unwrap_err().to_string().contains("48000"));
    }

    #[test]
    fn rejects_invalid_auth_environment_variable() {
        let error = config_with_stt(AuthConfig::Bearer {
            token_env: "not-valid".into(),
        })
        .validate()
        .unwrap_err();

        assert!(error.to_string().contains("environment variable name"));
    }

    #[test]
    fn serialize_default_config_without_provider_secrets() {
        let serialized = serde_json::to_string(&Config {
            recording: RecordingConfig::default(),
            stt: None,
            llm: None,
        })
        .unwrap();

        assert!(!serialized.contains("token"));
        assert!(!serialized.contains("key"));
    }
}
