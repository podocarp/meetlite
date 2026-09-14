use std::{
    collections::HashMap,
    env, fs,
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

const CONFIG_ENV: &str = "MEETLITE_CONFIG";
pub const STT_API_KEY_ENV: &str = "MEETLITE_STT_API_KEY";
pub const LLM_API_KEY_ENV: &str = "MEETLITE_LLM_API_KEY";
const KEYRING_SERVICE: &str = "Meetlite";
const LEGACY_KEYRING_SERVICE: &str = "meetlite";

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub recording: RecordingConfig,
    pub stt: Option<SttConfig>,
    pub llm: Option<LlmConfig>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            recording: RecordingConfig::default(),
            stt: Some(default_stt_config()),
            llm: Some(default_llm_config()),
        }
    }
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
    #[serde(default = "default_api_style")]
    pub api_style: ApiStyle,
    pub base_url: String,
    #[serde(default = "default_transcription_path")]
    pub transcription_path: String,
    pub model: String,
    pub language: Option<String>,
    pub prompt: Option<String>,
    #[serde(default = "default_response_format")]
    pub response_format: String,
    pub auth: AuthConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LlmConfig {
    #[serde(default = "default_api_style")]
    pub api_style: ApiStyle,
    pub base_url: String,
    #[serde(default = "default_chat_completions_path")]
    pub chat_completions_path: String,
    pub model: String,
    pub auth: AuthConfig,
    pub instructions: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ApiStyle {
    #[serde(alias = "openai-compatible")]
    OpenAiCompatible,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AuthConfig {
    None,
    Bearer {
        token_env: String,
    },
    BearerKeyring {
        service: String,
        user: String,
    },
    BearerPlain {
        token: String,
    },
    Header {
        header_name: String,
        value_env: String,
    },
}

impl AuthConfig {
    pub fn bearer_token(&self, override_env: &str) -> Result<Option<String>> {
        if let Some(token) = optional_environment_value(override_env)? {
            return Ok(Some(token));
        }

        match self {
            AuthConfig::None => Ok(None),
            AuthConfig::Bearer { token_env } => required_environment_value(token_env).map(Some),
            AuthConfig::BearerKeyring { service, user } => {
                let (service, user, allow_legacy) = keyring_spec(service, user);
                keyring_password(&service, &user, allow_legacy).map(Some)
            }
            AuthConfig::BearerPlain { token } if token.is_empty() => Ok(None),
            AuthConfig::BearerPlain { token } => Ok(Some(token.to_owned())),
            AuthConfig::Header { .. } => bail!("configured authentication is not a bearer token"),
        }
    }
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

        let contents = serde_json::to_string_pretty(&Self::default())? + "\n";
        let mut file = new_private_file(&path)?;
        file.write_all(contents.as_bytes())
            .with_context(|| format!("could not write configuration file {}", path.display()))?;
        set_private_file_permissions(&path)?;

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

    pub fn load_or_default(override_path: Option<&Path>) -> Result<Self> {
        Ok(Self::load_if_present(override_path)?.unwrap_or_default())
    }

    pub fn save(&self, override_path: Option<&Path>) -> Result<PathBuf> {
        self.validate()?;
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
        let contents = serde_json::to_string_pretty(self)? + "\n";
        let mut file = write_private_file(&path)?;
        file.write_all(contents.as_bytes())
            .with_context(|| format!("could not write configuration file {}", path.display()))?;
        set_private_file_permissions(&path)?;
        Ok(path)
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
            validate_api_style("stt", stt.api_style);
            validate_provider("stt", &stt.base_url, &stt.model, &stt.auth)?;
            if !stt.transcription_path.starts_with('/') {
                bail!("stt.transcription_path must start with /")
            }
            if stt.response_format.trim().is_empty() {
                bail!("stt.response_format must not be empty")
            }
        }
        if let Some(llm) = &self.llm {
            validate_api_style("llm", llm.api_style);
            validate_provider("llm", &llm.base_url, &llm.model, &llm.auth)?;
            if !llm.chat_completions_path.starts_with('/') {
                bail!("llm.chat_completions_path must start with /")
            }
        }

        Ok(())
    }
}

pub fn default_stt_config() -> SttConfig {
    SttConfig {
        api_style: default_api_style(),
        base_url: "https://api.openai.com/v1".into(),
        transcription_path: default_transcription_path(),
        model: "whisper-1".into(),
        language: None,
        prompt: None,
        response_format: default_response_format(),
        auth: AuthConfig::BearerPlain {
            token: String::new(),
        },
    }
}

pub fn default_llm_config() -> LlmConfig {
    LlmConfig {
        api_style: default_api_style(),
        base_url: "https://api.openai.com/v1".into(),
        chat_completions_path: default_chat_completions_path(),
        model: "gpt-4o-mini".into(),
        auth: AuthConfig::BearerPlain {
            token: String::new(),
        },
        instructions: None,
    }
}

pub fn store_bearer_auth(
    provider: &str,
    token: String,
    use_keyring: bool,
) -> (AuthConfig, Option<String>) {
    if token.is_empty() {
        return (AuthConfig::BearerPlain { token }, None);
    }

    if use_keyring {
        let service = KEYRING_SERVICE.to_string();
        let user = keyring_user(provider).to_string();
        announce_keyring_store(&service, &user);
        match store_keyring_password(&service, &user, &token) {
            Ok(()) => return (AuthConfig::BearerKeyring { service, user }, None),
            Err(error) => return (AuthConfig::BearerPlain { token }, Some(error.to_string())),
        }
    }

    (AuthConfig::BearerPlain { token }, None)
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

fn default_api_style() -> ApiStyle {
    ApiStyle::OpenAiCompatible
}

fn default_transcription_path() -> String {
    "/audio/transcriptions".into()
}

fn default_chat_completions_path() -> String {
    "/chat/completions".into()
}

fn validate_gain(name: &str, gain: f32) -> Result<()> {
    if !gain.is_finite() || gain < 0.0 {
        bail!("{name} must be a finite, non-negative number")
    }
    Ok(())
}

fn validate_api_style(_name: &str, api_style: ApiStyle) {
    match api_style {
        ApiStyle::OpenAiCompatible => {}
    }
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
        AuthConfig::BearerKeyring { service, user } => {
            if service.trim().is_empty() {
                bail!("{name}.auth.service must not be empty")
            }
            if user.trim().is_empty() {
                bail!("{name}.auth.user must not be empty")
            }
        }
        AuthConfig::BearerPlain { .. } => {}
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

fn required_environment_value(name: &str) -> Result<String> {
    let value = env::var(name)
        .with_context(|| format!("required environment variable {name} is not set"))?;
    configured_token(&value)
        .with_context(|| format!("required environment variable {name} is empty"))
}

fn optional_environment_value(name: &str) -> Result<Option<String>> {
    match env::var(name) {
        Ok(value) if value.is_empty() => Ok(None),
        Ok(value) => Ok(Some(value)),
        Err(env::VarError::NotPresent) => Ok(None),
        Err(error) => Err(error).with_context(|| format!("environment variable {name} is invalid")),
    }
}

fn configured_token(token: &str) -> Result<String> {
    if token.is_empty() {
        bail!("configured authentication token is empty")
    }
    Ok(token.to_owned())
}

fn keyring_user(provider: &str) -> &'static str {
    match provider {
        "stt" => "Meetlite STT API Key",
        "llm" => "Meetlite LLM API Key",
        _ => "Meetlite API Key",
    }
}

fn keyring_spec(service: &str, user: &str) -> (String, String, bool) {
    match (service, user) {
        (LEGACY_KEYRING_SERVICE, "stt-api-key") => (
            KEYRING_SERVICE.to_string(),
            keyring_user("stt").to_string(),
            true,
        ),
        (LEGACY_KEYRING_SERVICE, "llm-api-key") => (
            KEYRING_SERVICE.to_string(),
            keyring_user("llm").to_string(),
            true,
        ),
        (KEYRING_SERVICE, "Meetlite STT API Key" | "Meetlite LLM API Key") => {
            (service.to_string(), user.to_string(), true)
        }
        _ => (service.to_string(), user.to_string(), false),
    }
}

fn keyring_password(service: &str, user: &str, allow_legacy: bool) -> Result<String> {
    let cache_key = format!("{service}\0{user}");
    if let Some(result) = cached_keyring_password(&cache_key)? {
        return result;
    }

    announce_keyring_prompt(service, user);
    let result = match read_keyring_password(service, user) {
        Ok(token) => Ok(token),
        Err(error)
            if allow_legacy
                && service == KEYRING_SERVICE
                && error.to_string().contains("was not found") =>
        {
            let legacy_user = legacy_keyring_user(user);
            match read_keyring_password(LEGACY_KEYRING_SERVICE, legacy_user) {
                Ok(token) => {
                    let _ = store_keyring_password(service, user, &token);
                    Ok(token)
                }
                Err(legacy_error) => Err(legacy_error),
            }
        }
        Err(error) => Err(error),
    };
    cache_keyring_password(cache_key, &result)?;
    result
}

fn cached_keyring_password(cache_key: &str) -> Result<Option<Result<String>>> {
    let cache = keyring_cache()
        .lock()
        .map_err(|_| anyhow::anyhow!("keyring cache lock was poisoned"))?;
    Ok(cache.get(cache_key).map(|cached| match cached {
        CachedKeyringPassword::Token(token) => Ok(token.clone()),
        CachedKeyringPassword::Error(error) => Err(anyhow::anyhow!(error.clone())),
    }))
}

fn cache_keyring_password(cache_key: String, result: &Result<String>) -> Result<()> {
    let cached = match result {
        Ok(token) => CachedKeyringPassword::Token(token.clone()),
        Err(error) => CachedKeyringPassword::Error(error.to_string()),
    };
    keyring_cache()
        .lock()
        .map_err(|_| anyhow::anyhow!("keyring cache lock was poisoned"))?
        .insert(cache_key, cached);
    Ok(())
}

#[derive(Clone)]
enum CachedKeyringPassword {
    Token(String),
    Error(String),
}

fn keyring_cache() -> &'static Mutex<HashMap<String, CachedKeyringPassword>> {
    static CACHE: OnceLock<Mutex<HashMap<String, CachedKeyringPassword>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn announce_keyring_prompt(service: &str, user: &str) {
    crate::output::Output::new(false)
        .status("Keychain access needed", &format!("for {service}/{user}."));
    #[cfg(target_os = "macos")]
    crate::output::Output::new(false).instruction(
        "macOS may prompt for Keychain access; choose Allow or Always Allow to continue.",
    );
}

fn announce_keyring_store(service: &str, user: &str) {
    crate::output::Output::new(false).status("Storing API key", &format!("in {service}/{user}."));
    #[cfg(target_os = "macos")]
    crate::output::Output::new(false).instruction(
        "macOS may prompt for Keychain access; choose Allow or Always Allow to continue.",
    );
}

fn legacy_keyring_user(user: &str) -> &str {
    match user {
        "Meetlite STT API Key" => "stt-api-key",
        "Meetlite LLM API Key" => "llm-api-key",
        _ => user,
    }
}

fn read_keyring_password(service: &str, user: &str) -> Result<String> {
    let entry = keyring::v1::Entry::new(service, user)
        .with_context(|| format!("could not open keychain item {service}/{user}"))?;
    match entry.get_password() {
        Ok(token) => configured_token(&token),
        Err(error) => keyring_read_error(service, user, error),
    }
}

fn keyring_read_error(service: &str, user: &str, error: keyring::v1::Error) -> Result<String> {
    match error {
        keyring::v1::Error::NoStorageAccess(_) => {
            bail!("keychain access was denied for {service}/{user}; allow access in the prompt or set MEETLITE_STT_API_KEY or MEETLITE_LLM_API_KEY")
        }
        keyring::v1::Error::NoEntry => {
            bail!("keychain item {service}/{user} was not found; run `meetlite config setup stt` or `meetlite config setup llm` again")
        }
        other => {
            Err(other).with_context(|| format!("could not read keychain item {service}/{user}"))
        }
    }
}

fn store_keyring_password(service: &str, user: &str, token: &str) -> Result<()> {
    let entry = keyring::v1::Entry::new(service, user)
        .with_context(|| format!("could not open keychain item {service}/{user}"))?;
    entry
        .set_password(token)
        .with_context(|| format!("could not write keychain item {service}/{user}"))
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
fn set_private_file_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("could not restrict permissions on {}", path.display()))
}

#[cfg(not(unix))]
fn set_private_file_permissions(_path: &Path) -> Result<()> {
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

#[cfg(unix)]
fn write_private_file(path: &Path) -> Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;

    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("could not open configuration {}", path.display()))
}

#[cfg(not(unix))]
fn write_private_file(path: &Path) -> Result<fs::File> {
    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
        .with_context(|| format!("could not open configuration {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_with_stt(auth: AuthConfig) -> Config {
        Config {
            recording: RecordingConfig::default(),
            stt: Some(SttConfig {
                api_style: ApiStyle::OpenAiCompatible,
                base_url: "https://stt.example.test/v1".into(),
                transcription_path: default_transcription_path(),
                model: "whisper-test".into(),
                language: None,
                prompt: None,
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
    fn accepts_plain_bearer_provider() {
        config_with_stt(AuthConfig::BearerPlain {
            token: "test-token".into(),
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
    fn rejects_transcription_path_without_a_leading_slash() {
        let mut config = config_with_stt(AuthConfig::None);
        config.stt.as_mut().unwrap().transcription_path = "audio/transcriptions".into();

        assert!(config
            .validate()
            .unwrap_err()
            .to_string()
            .contains("transcription_path"));
    }

    #[test]
    fn default_config_includes_provider_defaults_without_secrets() {
        let config = Config::default();
        let stt = config.stt.unwrap();
        let llm = config.llm.unwrap();

        assert_eq!(stt.api_style, ApiStyle::OpenAiCompatible);
        assert_eq!(stt.base_url, "https://api.openai.com/v1");
        assert_eq!(stt.model, "whisper-1");
        assert_eq!(llm.api_style, ApiStyle::OpenAiCompatible);
        assert_eq!(llm.base_url, "https://api.openai.com/v1");
        assert_eq!(llm.model, "gpt-4o-mini");
        assert!(matches!(stt.auth, AuthConfig::BearerPlain { ref token } if token.is_empty()));
        assert!(matches!(llm.auth, AuthConfig::BearerPlain { ref token } if token.is_empty()));
    }

    #[cfg(unix)]
    #[test]
    fn save_uses_private_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        Config::default().save(Some(&path)).unwrap();

        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
