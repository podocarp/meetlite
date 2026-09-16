use std::{
    collections::HashMap,
    env, fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use tempfile::{Builder, NamedTempFile};

const CONFIG_ENV: &str = "MEETLITE_CONFIG";
pub const STT_API_KEY_ENV: &str = "MEETLITE_STT_API_KEY";
pub const LLM_API_KEY_ENV: &str = "MEETLITE_LLM_API_KEY";
const KEYRING_SERVICE: &str = "Meetlite";
const KEYRING_USER: &str = "Meetlite API Credentials";
const LEGACY_KEYRING_SERVICE: &str = "meetlite";
const LEGACY_STT_USER: &str = "Meetlite STT API Key";
const LEGACY_LLM_USER: &str = "Meetlite LLM API Key";

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub recording: RecordingConfig,
    #[serde(default = "default_summary_enabled")]
    pub summary_enabled: bool,
    pub stt: Option<SttConfig>,
    pub llm: Option<LlmConfig>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            recording: RecordingConfig::default(),
            summary_enabled: default_summary_enabled(),
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
    #[serde(default, rename = "microphone_gain", skip_serializing)]
    _legacy_microphone_gain: Option<f32>,
    #[serde(default, rename = "system_gain", skip_serializing)]
    _legacy_system_gain: Option<f32>,
    pub microphone_device: Option<String>,
    pub system_device: Option<String>,
}

impl Default for RecordingConfig {
    fn default() -> Self {
        Self {
            sample_rate: default_sample_rate(),
            _legacy_microphone_gain: None,
            _legacy_system_gain: None,
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
    #[serde(default)]
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigApplyInput {
    pub stt: ProviderApplyInput,
    pub summary: SummaryApplyInput,
    #[serde(skip)]
    replacement: Option<Config>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SummaryApplyInput {
    pub enabled: bool,
    pub llm: LlmApplyInput,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderApplyInput {
    pub base_url: String,
    pub model: String,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub remove_saved_key: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LlmApplyInput {
    pub base_url: String,
    pub model: String,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub remove_saved_key: bool,
    #[serde(default)]
    pub instructions: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ConfigStatus {
    pub path: String,
    pub usable: bool,
    pub reason: Option<String>,
    pub invalid_config_will_be_backed_up: bool,
    pub summary_enabled: bool,
    pub stt: ProviderStatus,
    pub llm: LlmStatus,
}

#[derive(Debug, Serialize)]
pub struct ProviderStatus {
    pub base_url: String,
    pub model: String,
    pub prompt: Option<String>,
    pub credential_configured: bool,
    pub managed_credential_configured: Option<bool>,
    pub auth_source: &'static str,
    pub auth_provenance: &'static str,
}

#[derive(Debug, Serialize)]
pub struct LlmStatus {
    pub base_url: String,
    pub model: String,
    pub credential_configured: bool,
    pub managed_credential_configured: Option<bool>,
    pub auth_source: &'static str,
    pub auth_provenance: &'static str,
    pub instructions: Option<String>,
}

#[derive(Debug)]
pub struct ConfigApplyResult {
    pub path: PathBuf,
    pub backup_path: Option<PathBuf>,
    pub stored_credentials: Vec<&'static str>,
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
    pub fn bearer_token(&self, override_env: &str, provider: &str) -> Result<Option<String>> {
        if let Some(token) = optional_environment_value(override_env)? {
            return Ok(Some(token));
        }

        match self {
            AuthConfig::None => Ok(None),
            AuthConfig::Bearer { token_env } => required_environment_value(token_env).map(Some),
            AuthConfig::BearerKeyring { service, user } => {
                if service == KEYRING_SERVICE && user == KEYRING_USER {
                    return managed_keyring_credentials()?
                        .token(provider)
                        .map(Some)
                        .with_context(|| {
                            format!("keychain item {service}/{user} does not contain a {provider} credential")
                        });
                }
                if is_legacy_meetlite_keyring(service, user) {
                    return legacy_keyring_password(service, user).map(Some);
                }
                keyring_password(service, user).map(Some)
            }
            AuthConfig::BearerPlain { token } if token.is_empty() => Ok(None),
            AuthConfig::BearerPlain { token } => Ok(Some(token.to_owned())),
            AuthConfig::Header { .. } => bail!("configured authentication is not a bearer token"),
        }
    }
}

fn is_legacy_meetlite_keyring(service: &str, user: &str) -> bool {
    matches!(
        (service, user),
        (KEYRING_SERVICE, LEGACY_STT_USER | LEGACY_LLM_USER)
            | (LEGACY_KEYRING_SERVICE, "stt-api-key" | "llm-api-key")
    )
}

#[cfg(test)]
fn legacy_keyring_users(provider: &str) -> (&'static str, &'static str) {
    match provider {
        "stt" => (LEGACY_STT_USER, "stt-api-key"),
        "llm" => (LEGACY_LLM_USER, "llm-api-key"),
        _ => ("Meetlite API Key", "api-key"),
    }
}

fn legacy_keyring_password(service: &str, user: &str) -> Result<String> {
    let (primary_user, fallback_user) = match user {
        LEGACY_STT_USER | "stt-api-key" => (LEGACY_STT_USER, "stt-api-key"),
        LEGACY_LLM_USER | "llm-api-key" => (LEGACY_LLM_USER, "llm-api-key"),
        _ => return keyring_password(service, user),
    };
    match read_keyring_password(KEYRING_SERVICE, primary_user) {
        Ok(token) => Ok(token),
        Err(error) if error.to_string().contains("was not found") => {
            read_keyring_password(LEGACY_KEYRING_SERVICE, fallback_user)
        }
        Err(error) => Err(error),
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
        prepare_config_write(&Self::default(), &path)?.commit_new()
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

    pub fn load_for_update(override_path: Option<&Path>) -> Result<Self> {
        match Self::load_if_present(override_path) {
            Ok(Some(config)) => Ok(config),
            Ok(None) | Err(_) => Ok(Self::default()),
        }
    }

    #[cfg(test)]
    pub fn save(&self, override_path: Option<&Path>) -> Result<PathBuf> {
        self.validate()?;
        let path = Self::path(override_path)?;
        prepare_config_write(self, &path)?.commit()
    }

    pub fn stt(&self) -> Result<&SttConfig> {
        self.stt.as_ref().context(
            "no STT provider is configured; add an `stt` section to the Meetlite configuration",
        )
    }

    pub fn status(
        override_path: Option<&Path>,
        store: &dyn ManagedCredentialStore,
    ) -> Result<ConfigStatus> {
        let path = Self::path(override_path)?;
        let defaults = Self::default();
        let loaded = match Self::load_if_present(Some(&path)) {
            Ok(Some(config)) => config,
            Ok(None) => return Ok(status_for_missing(path, defaults)),
            Err(_) => return Ok(status_for_invalid(path, defaults)),
        };
        status_for_config(path, loaded, store)
    }

    pub fn apply(
        override_path: Option<&Path>,
        input: ConfigApplyInput,
        store: &dyn ManagedCredentialStore,
    ) -> Result<ConfigApplyResult> {
        Self::apply_with_commit(override_path, input, store, PreparedConfigWrite::commit)
    }

    fn apply_with_commit<F>(
        override_path: Option<&Path>,
        input: ConfigApplyInput,
        store: &dyn ManagedCredentialStore,
        commit: F,
    ) -> Result<ConfigApplyResult>
    where
        F: FnOnce(PreparedConfigWrite) -> Result<PathBuf>,
    {
        validate_key_change(
            "stt",
            input.stt.api_key.as_deref(),
            input.stt.remove_saved_key,
        )?;
        validate_key_change(
            "summary.llm",
            input.summary.llm.api_key.as_deref(),
            input.summary.llm.remove_saved_key,
        )?;

        let stt_key = nonblank_key(input.stt.api_key);
        let llm_key = nonblank_key(input.summary.llm.api_key);
        let stt_stored = stt_key.is_some();
        let llm_stored = llm_key.is_some();
        let path = Self::path(override_path)?;
        let replacement = input.replacement;
        let (mut config, invalid_existing) = match Self::load_if_present(Some(&path)) {
            Ok(Some(config)) => (replacement.unwrap_or(config), false),
            Ok(None) => (replacement.unwrap_or_default(), false),
            Err(_) => (replacement.unwrap_or_default(), true),
        };
        let mut stt = config.stt.take().unwrap_or_else(default_stt_config);
        stt.base_url = input.stt.base_url;
        stt.model = input.stt.model;
        stt.prompt = input
            .stt
            .prompt
            .and_then(|value| (!value.trim().is_empty()).then_some(value));
        let mut llm = config.llm.take().unwrap_or_else(default_llm_config);
        llm.base_url = input.summary.llm.base_url;
        llm.model = input.summary.llm.model;
        llm.instructions = input
            .summary
            .llm
            .instructions
            .and_then(|value| (!value.trim().is_empty()).then_some(value));
        config.summary_enabled = input.summary.enabled;
        config.stt = Some(stt.clone());
        config.llm = Some(llm.clone());
        config.validate()?;

        let stt_managed_removal = input.stt.remove_saved_key && auth_uses_managed_store(&stt.auth);
        let llm_managed_removal =
            input.summary.llm.remove_saved_key && auth_uses_managed_store(&llm.auth);
        let changes_requested =
            stt_stored || llm_stored || stt_managed_removal || llm_managed_removal;
        let original_credentials = if changes_requested {
            Some(store.load()?)
        } else {
            None
        };
        let mut credentials = original_credentials
            .as_ref()
            .and_then(|credentials| credentials.clone())
            .unwrap_or_default();
        apply_key_change(
            &mut credentials.stt,
            stt_key,
            stt_managed_removal,
            &mut stt.auth,
        );
        apply_key_change(
            &mut credentials.llm,
            llm_key,
            llm_managed_removal,
            &mut llm.auth,
        );
        config.stt = Some(stt);
        config.llm = Some(llm);
        config.validate()?;

        let candidate = prepare_config_write(&config, &path)?;
        let backup = if invalid_existing {
            Some(backup_invalid_config(&path)?)
        } else {
            None
        };
        if changes_requested {
            if let Err(store_error) = store_credentials(store, &credentials) {
                let mut error = store_error;
                if let Some(original) = original_credentials.as_ref() {
                    if let Err(rollback_error) = restore_credentials(store, original.as_ref()) {
                        error = error
                            .context(format!("credential rollback also failed: {rollback_error}"));
                    }
                }
                if let Err(rollback_error) = restore_invalid_config_backup(&path, backup.as_deref())
                {
                    error = error.context(format!(
                        "configuration rollback also failed: {rollback_error}"
                    ));
                }
                return Err(error);
            }
        }
        if let Err(commit_error) = commit(candidate) {
            let mut error = commit_error;
            if let Some(original) = original_credentials {
                if let Err(rollback_error) = restore_credentials(store, original.as_ref()) {
                    error =
                        error.context(format!("credential rollback also failed: {rollback_error}"));
                }
            }
            if let Err(rollback_error) = restore_invalid_config_backup(&path, backup.as_deref()) {
                error = error.context(format!(
                    "configuration rollback also failed: {rollback_error}"
                ));
            }
            return Err(error);
        }

        let mut stored_credentials = Vec::new();
        if stt_stored {
            stored_credentials.push("stt");
        }
        if llm_stored {
            stored_credentials.push("llm");
        }
        Ok(ConfigApplyResult {
            path,
            backup_path: backup,
            stored_credentials,
        })
    }

    pub(crate) fn validate(&self) -> Result<()> {
        if self.recording.sample_rate != 48_000 {
            bail!("recording.sample_rate must be 48000 for the initial recorder")
        }
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

fn status_for_missing(path: PathBuf, defaults: Config) -> ConfigStatus {
    let stt = defaults.stt.expect("default STT configuration");
    let llm = defaults.llm.expect("default LLM configuration");
    ConfigStatus {
        path: path.display().to_string(),
        usable: false,
        reason: Some("missing_config".into()),
        invalid_config_will_be_backed_up: false,
        summary_enabled: defaults.summary_enabled,
        stt: ProviderStatus {
            base_url: stt.base_url,
            model: stt.model,
            prompt: stt.prompt,
            credential_configured: false,
            managed_credential_configured: None,
            auth_source: "none",
            auth_provenance: "default",
        },
        llm: LlmStatus {
            base_url: llm.base_url,
            model: llm.model,
            credential_configured: false,
            managed_credential_configured: None,
            auth_source: "none",
            auth_provenance: "default",
            instructions: llm.instructions,
        },
    }
}

fn status_for_invalid(path: PathBuf, defaults: Config) -> ConfigStatus {
    let mut status = status_for_missing(path, defaults);
    status.reason = Some("invalid_config".into());
    status.invalid_config_will_be_backed_up = true;
    status
}

fn status_for_config(
    path: PathBuf,
    config: Config,
    store: &dyn ManagedCredentialStore,
) -> Result<ConfigStatus> {
    status_for_config_with_env(path, config, store, environment_credential_configured)
}

fn status_for_config_with_env<F>(
    path: PathBuf,
    config: Config,
    store: &dyn ManagedCredentialStore,
    environment_configured: F,
) -> Result<ConfigStatus>
where
    F: Fn(&str) -> bool + Copy,
{
    let defaults = Config::default();
    let stt_missing = config.stt.is_none();
    let llm_missing = config.llm.is_none();
    let stt = config
        .stt
        .unwrap_or_else(|| defaults.stt.expect("default STT configuration"));
    let llm = config
        .llm
        .unwrap_or_else(|| defaults.llm.expect("default LLM configuration"));
    let mut managed: Option<(Option<ManagedKeyringCredentials>, bool)> = None;
    let stt_status = credential_status(
        &stt.auth,
        "stt",
        STT_API_KEY_ENV,
        true,
        store,
        &mut managed,
        environment_configured,
    );
    let llm_status = credential_status(
        &llm.auth,
        "llm",
        LLM_API_KEY_ENV,
        config.summary_enabled,
        store,
        &mut managed,
        environment_configured,
    );
    let store_unavailable = stt_status.store_unavailable || llm_status.store_unavailable;
    let usable = !stt_missing
        && stt_status.usable
        && (!config.summary_enabled || !llm_missing && llm_status.usable);
    let reason = if stt_missing {
        Some("missing_stt".into())
    } else if config.summary_enabled && llm_missing {
        Some("missing_llm".into())
    } else if store_unavailable {
        Some("credential_store_unavailable".into())
    } else if !usable {
        Some("required_credentials_missing".into())
    } else {
        None
    };
    Ok(ConfigStatus {
        path: path.display().to_string(),
        usable,
        reason,
        invalid_config_will_be_backed_up: false,
        summary_enabled: config.summary_enabled,
        stt: ProviderStatus {
            base_url: stt.base_url,
            model: stt.model,
            prompt: stt.prompt,
            credential_configured: stt_status.configured,
            managed_credential_configured: stt_status.managed_configured,
            auth_source: stt_status.source,
            auth_provenance: stt_status.provenance,
        },
        llm: LlmStatus {
            base_url: llm.base_url,
            model: llm.model,
            credential_configured: llm_status.configured,
            managed_credential_configured: llm_status.managed_configured,
            auth_source: llm_status.source,
            auth_provenance: llm_status.provenance,
            instructions: llm.instructions,
        },
    })
}

struct CredentialStatus {
    usable: bool,
    configured: bool,
    managed_configured: Option<bool>,
    source: &'static str,
    provenance: &'static str,
    store_unavailable: bool,
}

fn credential_status<F>(
    auth: &AuthConfig,
    provider: &str,
    override_env: &str,
    active: bool,
    store: &dyn ManagedCredentialStore,
    managed: &mut Option<(Option<ManagedKeyringCredentials>, bool)>,
    environment_configured: F,
) -> CredentialStatus
where
    F: Fn(&str) -> bool,
{
    let override_applies = matches!(
        auth,
        AuthConfig::BearerKeyring { .. } | AuthConfig::BearerPlain { .. }
    );
    if override_applies && environment_configured(override_env) {
        return CredentialStatus {
            usable: true,
            configured: true,
            managed_configured: None,
            source: "environment",
            provenance: "runtime_override",
            store_unavailable: false,
        };
    }
    match auth {
        AuthConfig::None => credential_status_value(true, false, None, "none", "configured"),
        AuthConfig::Bearer { token_env } => {
            let configured = environment_configured(token_env);
            credential_status_value(configured, configured, None, "environment", "configured")
        }
        AuthConfig::BearerKeyring { service, user }
            if service == KEYRING_SERVICE && user == KEYRING_USER =>
        {
            if !active {
                return credential_status_value(true, false, None, "managed_keyring", "managed");
            }
            let (configured, unavailable) = match managed.as_ref() {
                Some((loaded, unavailable)) => (
                    loaded
                        .as_ref()
                        .and_then(|credentials| credentials.token(provider))
                        .is_some(),
                    *unavailable,
                ),
                None => match store.load() {
                    Ok(loaded) => {
                        let configured = loaded
                            .as_ref()
                            .and_then(|credentials| credentials.token(provider))
                            .is_some();
                        *managed = Some((loaded, false));
                        (configured, false)
                    }
                    Err(_) => {
                        *managed = Some((None, true));
                        (false, true)
                    }
                },
            };
            CredentialStatus {
                usable: configured,
                configured,
                managed_configured: (!unavailable).then_some(configured),
                source: "managed_keyring",
                provenance: "managed",
                store_unavailable: unavailable,
            }
        }
        AuthConfig::BearerKeyring { service, user }
            if is_legacy_meetlite_keyring(service, user) =>
        {
            keyring_credential_status(store, user, active)
        }
        AuthConfig::BearerKeyring { service, user } => {
            if !active {
                return credential_status_value(true, false, None, "custom_keyring", "configured");
            }
            match store.load_keyring(service, user) {
                Ok(value) => {
                    let configured = value.is_some();
                    credential_status_value(
                        configured,
                        configured,
                        None,
                        "custom_keyring",
                        "configured",
                    )
                }
                Err(_) => credential_store_error("custom_keyring", "configured"),
            }
        }
        AuthConfig::BearerPlain { token } => {
            let configured = !token.is_empty();
            credential_status_value(configured, configured, None, "plaintext", "configured")
        }
        AuthConfig::Header { value_env, .. } => {
            let configured = environment_configured(value_env);
            credential_status_value(configured, configured, None, "environment", "custom_header")
        }
    }
}

fn credential_status_value(
    usable: bool,
    configured: bool,
    managed_configured: Option<bool>,
    source: &'static str,
    provenance: &'static str,
) -> CredentialStatus {
    CredentialStatus {
        usable,
        configured,
        managed_configured,
        source,
        provenance,
        store_unavailable: false,
    }
}

fn credential_store_error(source: &'static str, provenance: &'static str) -> CredentialStatus {
    CredentialStatus {
        usable: false,
        configured: false,
        managed_configured: None,
        source,
        provenance,
        store_unavailable: true,
    }
}

fn keyring_credential_status(
    store: &dyn ManagedCredentialStore,
    user: &str,
    active: bool,
) -> CredentialStatus {
    let provenance = "legacy_meetlite";
    if !active {
        return credential_status_value(true, false, None, "legacy_keyring", provenance);
    }
    let (primary_user, fallback_user) = match user {
        LEGACY_STT_USER | "stt-api-key" => (LEGACY_STT_USER, "stt-api-key"),
        LEGACY_LLM_USER | "llm-api-key" => (LEGACY_LLM_USER, "llm-api-key"),
        _ => (user, user),
    };
    match store.load_keyring(KEYRING_SERVICE, primary_user) {
        Ok(Some(_)) => credential_status_value(true, true, None, "legacy_keyring", provenance),
        Ok(None) => match store.load_keyring(LEGACY_KEYRING_SERVICE, fallback_user) {
            Ok(value) => {
                let configured = value.is_some();
                credential_status_value(configured, configured, None, "legacy_keyring", provenance)
            }
            Err(_) => credential_store_error("legacy_keyring", provenance),
        },
        Err(_) => credential_store_error("legacy_keyring", provenance),
    }
}

fn environment_credential_configured(name: &str) -> bool {
    env::var(name).is_ok_and(|value| !value.is_empty())
}

fn validate_key_change(provider: &str, key: Option<&str>, remove: bool) -> Result<()> {
    if remove && key.is_some_and(|value| !value.trim().is_empty()) {
        bail!("{provider}.api_key and {provider}.remove_saved_key cannot both be set")
    }
    Ok(())
}

fn nonblank_key(key: Option<String>) -> Option<String> {
    key.and_then(|value| (!value.trim().is_empty()).then_some(value))
}

fn apply_key_change(
    credential: &mut Option<String>,
    replacement: Option<String>,
    remove: bool,
    auth: &mut AuthConfig,
) {
    if let Some(replacement) = replacement {
        *credential = Some(replacement);
        *auth = managed_auth();
    } else if remove {
        *credential = None;
        if auth_uses_managed_store(auth) {
            *auth = AuthConfig::None;
        }
    }
}

fn store_credentials(
    store: &dyn ManagedCredentialStore,
    credentials: &ManagedKeyringCredentials,
) -> Result<()> {
    if credentials.stt.is_none() && credentials.llm.is_none() {
        store.delete()
    } else {
        store.save(credentials)
    }
}

fn restore_credentials(
    store: &dyn ManagedCredentialStore,
    credentials: Option<&ManagedKeyringCredentials>,
) -> Result<()> {
    match credentials {
        Some(credentials) => store.save(credentials),
        None => store.delete(),
    }
}

fn auth_uses_managed_store(auth: &AuthConfig) -> bool {
    matches!(auth, AuthConfig::BearerKeyring { service, user } if service == KEYRING_SERVICE && user == KEYRING_USER)
}

fn managed_auth() -> AuthConfig {
    AuthConfig::BearerKeyring {
        service: KEYRING_SERVICE.into(),
        user: KEYRING_USER.into(),
    }
}

pub(crate) trait ManagedCredentialStore {
    fn load(&self) -> Result<Option<ManagedKeyringCredentials>>;
    fn save(&self, credentials: &ManagedKeyringCredentials) -> Result<()>;
    fn delete(&self) -> Result<()>;
    fn load_keyring(&self, service: &str, user: &str) -> Result<Option<String>>;
}

pub struct KeyringManagedCredentialStore;

impl ManagedCredentialStore for KeyringManagedCredentialStore {
    fn load(&self) -> Result<Option<ManagedKeyringCredentials>> {
        announce_keyring_prompt(KEYRING_SERVICE, KEYRING_USER);
        let entry = keyring::v1::Entry::new(KEYRING_SERVICE, KEYRING_USER)
            .context("could not open managed credential store")?;
        match entry.get_password() {
            Ok(value) => serde_json::from_str(&value)
                .context("managed credential store contains invalid data")
                .map(Some),
            Err(keyring::v1::Error::NoEntry) => Ok(None),
            Err(_) => bail!("managed credential store is unavailable"),
        }
    }

    fn save(&self, credentials: &ManagedKeyringCredentials) -> Result<()> {
        announce_keyring_store(KEYRING_SERVICE, KEYRING_USER);
        let value = serde_json::to_string(credentials)?;
        let entry = keyring::v1::Entry::new(KEYRING_SERVICE, KEYRING_USER)
            .context("could not open managed credential store")?;
        entry
            .set_password(&value)
            .map_err(|_| anyhow::anyhow!("managed credential store is unavailable"))
    }

    fn delete(&self) -> Result<()> {
        announce_keyring_store(KEYRING_SERVICE, KEYRING_USER);
        let entry = keyring::v1::Entry::new(KEYRING_SERVICE, KEYRING_USER)
            .context("could not open managed credential store")?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::v1::Error::NoEntry) => Ok(()),
            Err(_) => bail!("managed credential store is unavailable"),
        }
    }

    fn load_keyring(&self, service: &str, user: &str) -> Result<Option<String>> {
        announce_keyring_prompt(service, user);
        let entry =
            keyring::v1::Entry::new(service, user).context("could not open credential store")?;
        match entry.get_password() {
            Ok(value) if value.is_empty() => Ok(None),
            Ok(value) => Ok(Some(value)),
            Err(keyring::v1::Error::NoEntry) => Ok(None),
            Err(_) => bail!("credential store is unavailable"),
        }
    }
}

pub fn setup_provider(
    override_path: Option<&Path>,
    provider: &str,
    config: Config,
    token: String,
) -> Result<ConfigApplyResult> {
    let stt = config
        .stt
        .as_ref()
        .cloned()
        .unwrap_or_else(default_stt_config);
    let llm = config
        .llm
        .as_ref()
        .cloned()
        .unwrap_or_else(default_llm_config);
    let mut input = ConfigApplyInput {
        stt: ProviderApplyInput {
            base_url: stt.base_url,
            model: stt.model,
            prompt: stt.prompt,
            api_key: None,
            remove_saved_key: false,
        },
        summary: SummaryApplyInput {
            enabled: config.summary_enabled,
            llm: LlmApplyInput {
                base_url: llm.base_url,
                model: llm.model,
                api_key: None,
                remove_saved_key: false,
                instructions: llm.instructions,
            },
        },
        replacement: Some(config),
    };
    if !token.is_empty() {
        match provider {
            "stt" => input.stt.api_key = Some(token),
            "llm" => input.summary.llm.api_key = Some(token),
            _ => bail!("unknown provider"),
        }
    }
    Config::apply(override_path, input, &KeyringManagedCredentialStore)
}

fn default_summary_enabled() -> bool {
    true
}

fn default_sample_rate() -> u32 {
    48_000
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

fn keyring_password(service: &str, user: &str) -> Result<String> {
    let cache_key = format!("{service}\0{user}");
    if let Some(result) = cached_keyring_password(&cache_key)? {
        return result;
    }

    announce_keyring_prompt(service, user);
    let result = read_keyring_password(service, user);
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

#[derive(Clone, Default, Deserialize, Serialize)]
pub(crate) struct ManagedKeyringCredentials {
    stt: Option<String>,
    llm: Option<String>,
}

impl ManagedKeyringCredentials {
    fn token(&self, provider: &str) -> Option<String> {
        match provider {
            "stt" => self.stt.clone(),
            "llm" => self.llm.clone(),
            _ => None,
        }
    }

    #[cfg(test)]
    fn set(&mut self, provider: &str, token: String) {
        match provider {
            "stt" => self.stt = Some(token),
            "llm" => self.llm = Some(token),
            _ => {}
        }
    }
}

fn managed_keyring_credentials() -> Result<ManagedKeyringCredentials> {
    let token = keyring_password(KEYRING_SERVICE, KEYRING_USER)?;
    serde_json::from_str(&token).context("managed keychain credentials are not valid JSON")
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

fn backup_invalid_config(path: &Path) -> Result<PathBuf> {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .context("configuration path must have a valid file name")?;
    for suffix in 0.. {
        let backup_name = if suffix == 0 {
            format!("{file_name}.invalid.bak")
        } else {
            format!("{file_name}.invalid.{suffix}.bak")
        };
        let backup = path.with_file_name(backup_name);
        if !backup.exists() {
            fs::rename(path, &backup).with_context(|| {
                format!("could not back up invalid configuration {}", path.display())
            })?;
            sync_parent_directory(path)?;
            return Ok(backup);
        }
    }
    unreachable!()
}

fn restore_invalid_config_backup(path: &Path, backup: Option<&Path>) -> Result<()> {
    let Some(backup) = backup else {
        return Ok(());
    };
    if path.exists() {
        fs::remove_file(path).with_context(|| {
            format!(
                "could not remove replacement configuration {}",
                path.display()
            )
        })?;
    }
    fs::rename(backup, path)
        .with_context(|| format!("could not restore invalid configuration {}", path.display()))?;
    sync_parent_directory(path)
}

struct PreparedConfigWrite {
    path: PathBuf,
    temporary: NamedTempFile,
}

impl PreparedConfigWrite {
    fn commit(self) -> Result<PathBuf> {
        let Self { path, temporary } = self;
        temporary
            .persist(&path)
            .map_err(|error| error.error)
            .with_context(|| {
                format!(
                    "could not atomically write configuration {}",
                    path.display()
                )
            })?;
        Ok(path)
    }

    fn commit_new(self) -> Result<PathBuf> {
        let Self { path, temporary } = self;
        temporary
            .persist_noclobber(&path)
            .map_err(|error| error.error)
            .with_context(|| {
                format!(
                    "refusing to overwrite existing configuration {}",
                    path.display()
                )
            })?;
        set_private_file_permissions(&path)?;
        sync_parent_directory(&path)?;
        Ok(path)
    }
}

fn prepare_config_write(config: &Config, path: &Path) -> Result<PreparedConfigWrite> {
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
    let contents = serde_json::to_string_pretty(config)? + "\n";
    let mut temporary = Builder::new()
        .prefix(".meetlite-config-")
        .tempfile_in(parent)
        .with_context(|| format!("could not prepare configuration in {}", parent.display()))?;
    set_private_file_permissions(temporary.path())?;
    temporary
        .write_all(contents.as_bytes())
        .with_context(|| format!("could not prepare configuration file {}", path.display()))?;
    temporary
        .as_file_mut()
        .sync_all()
        .with_context(|| format!("could not sync configuration file {}", path.display()))?;
    Ok(PreparedConfigWrite {
        path: path.to_path_buf(),
        temporary,
    })
}

#[cfg(unix)]
fn sync_parent_directory(path: &Path) -> Result<()> {
    let parent = path
        .parent()
        .context("configuration path must include a parent directory")?;
    fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .with_context(|| {
            format!(
                "could not sync configuration directory {}",
                parent.display()
            )
        })
}

#[cfg(not(unix))]
fn sync_parent_directory(_path: &Path) -> Result<()> {
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
fn set_private_file_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("could not restrict permissions on {}", path.display()))
}

#[cfg(not(unix))]
fn set_private_file_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, rc::Rc};

    use super::*;

    #[derive(Default)]
    struct FakeStoreState {
        credentials: Option<ManagedKeyringCredentials>,
        keyring: HashMap<(String, String), String>,
        operations: Vec<&'static str>,
        unavailable: bool,
    }

    #[derive(Clone, Default)]
    struct FakeStore(Rc<RefCell<FakeStoreState>>);

    impl ManagedCredentialStore for FakeStore {
        fn load(&self) -> Result<Option<ManagedKeyringCredentials>> {
            let mut state = self.0.borrow_mut();
            state.operations.push("load");
            if state.unavailable {
                bail!("store unavailable")
            }
            Ok(state.credentials.clone())
        }

        fn save(&self, credentials: &ManagedKeyringCredentials) -> Result<()> {
            let mut state = self.0.borrow_mut();
            state.operations.push("save");
            if state.unavailable {
                bail!("store unavailable")
            }
            state.credentials = Some(credentials.clone());
            Ok(())
        }

        fn delete(&self) -> Result<()> {
            let mut state = self.0.borrow_mut();
            state.operations.push("delete");
            if state.unavailable {
                bail!("store unavailable")
            }
            state.credentials = None;
            Ok(())
        }

        fn load_keyring(&self, service: &str, user: &str) -> Result<Option<String>> {
            let mut state = self.0.borrow_mut();
            state.operations.push("load_keyring");
            if state.unavailable {
                bail!("store unavailable")
            }
            Ok(state
                .keyring
                .get(&(service.to_owned(), user.to_owned()))
                .cloned())
        }
    }

    fn apply_input(stt_key: Option<&str>, llm_key: Option<&str>) -> ConfigApplyInput {
        serde_json::from_value(serde_json::json!({
            "stt": {
                "base_url": "https://stt.example.test/v1",
                "model": "stt-model",
                "prompt": "Names: Meetlite, Nia Chen",
                "api_key": stt_key,
                "remove_saved_key": false
            },
            "summary": {
                "enabled": true,
                "llm": {
                    "base_url": "https://llm.example.test/v1",
                    "model": "llm-model",
                    "api_key": llm_key,
                    "remove_saved_key": false,
                    "instructions": "Use concise bullets."
                }
            }
        }))
        .unwrap()
    }

    fn config_with_stt(auth: AuthConfig) -> Config {
        Config {
            recording: RecordingConfig::default(),
            summary_enabled: true,
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
    fn apply_payload_is_strict_and_rejects_conflicting_key_actions() {
        let unknown = serde_json::from_value::<ConfigApplyInput>(serde_json::json!({
            "stt": {"base_url": "https://stt.test", "model": "stt", "extra": true},
            "summary": {"enabled": true, "llm": {"base_url": "https://llm.test", "model": "llm"}}
        }))
        .err()
        .unwrap();
        assert!(unknown.to_string().contains("unknown field"));

        let missing = serde_json::from_value::<ConfigApplyInput>(serde_json::json!({
            "stt": {"base_url": "https://stt.test", "model": "stt"},
            "summary": {"enabled": true}
        }))
        .err()
        .unwrap();
        assert!(missing.to_string().contains("missing field"));

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        let mut input = apply_input(Some("secret-value"), None);
        input.stt.remove_saved_key = true;
        let error = Config::apply(Some(&path), input, &FakeStore::default()).unwrap_err();
        assert!(!error.to_string().contains("secret-value"));
    }

    #[test]
    fn blank_keys_preserve_without_accessing_store() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        let store = FakeStore::default();
        Config::apply(Some(&path), apply_input(Some("  "), None), &store).unwrap();

        assert!(store.0.borrow().operations.is_empty());
        let saved = fs::read_to_string(path).unwrap();
        assert!(!saved.contains("api_key"));
        assert!(!saved.contains("secret"));
    }

    #[test]
    fn blank_keys_and_remove_saved_key_preserve_custom_auth() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        let mut config = Config::default();
        config.stt.as_mut().unwrap().auth = AuthConfig::Header {
            header_name: "X-API-Key".into(),
            value_env: "CUSTOM_STT_KEY".into(),
        };
        config.save(Some(&path)).unwrap();
        let store = FakeStore::default();

        let mut input = apply_input(Some(" \t "), None);
        input.stt.remove_saved_key = true;
        Config::apply(Some(&path), input, &store).unwrap();

        let saved = Config::load(Some(&path)).unwrap();
        assert!(matches!(
            saved.stt.unwrap().auth,
            AuthConfig::Header {
                ref header_name,
                ref value_env
            } if header_name == "X-API-Key" && value_env == "CUSTOM_STT_KEY"
        ));
        assert!(store.0.borrow().operations.is_empty());
        assert!(!fs::read_to_string(path).unwrap().contains("api_key"));
    }

    #[test]
    fn replacements_are_batched_and_preserve_sibling_key() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        let store = FakeStore::default();
        store.0.borrow_mut().credentials = Some(ManagedKeyringCredentials {
            stt: None,
            llm: Some("existing-llm-secret".into()),
        });

        let result = Config::apply(
            Some(&path),
            apply_input(Some("new-stt-secret"), None),
            &store,
        )
        .unwrap();
        let state = store.0.borrow();
        assert_eq!(state.operations, ["load", "save"]);
        let credentials = state.credentials.as_ref().unwrap();
        assert_eq!(credentials.stt.as_deref(), Some("new-stt-secret"));
        assert_eq!(credentials.llm.as_deref(), Some("existing-llm-secret"));
        assert_eq!(result.stored_credentials, ["stt"]);
        let saved = fs::read_to_string(path).unwrap();
        assert!(!saved.contains("new-stt-secret"));
        assert!(!saved.contains("existing-llm-secret"));
    }

    #[test]
    fn both_replacements_use_one_store_batch() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        let store = FakeStore::default();
        Config::apply(
            Some(&path),
            apply_input(Some("stt-secret"), Some("llm-secret")),
            &store,
        )
        .unwrap();

        let state = store.0.borrow();
        assert_eq!(state.operations, ["load", "save"]);
        let credentials = state.credentials.as_ref().unwrap();
        assert_eq!(credentials.stt.as_deref(), Some("stt-secret"));
        assert_eq!(credentials.llm.as_deref(), Some("llm-secret"));
    }

    #[test]
    fn malformed_existing_config_is_backed_up_and_replaced() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        fs::write(&path, "{ malformed secret-value").unwrap();
        let store = FakeStore::default();

        let result = Config::apply(Some(&path), apply_input(None, None), &store).unwrap();

        let backup = result.backup_path.as_ref().unwrap();
        assert!(backup.exists());
        assert_eq!(
            fs::read_to_string(&backup).unwrap(),
            "{ malformed secret-value"
        );
        assert!(Config::load(Some(&path)).is_ok());
        let serialized = format!("{result:?}");
        assert!(!serialized.contains("secret-value"));
    }

    #[test]
    fn malformed_existing_config_is_restored_when_commit_fails() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        fs::write(&path, "{ malformed secret-value").unwrap();
        let store = FakeStore::default();

        assert!(Config::apply_with_commit(
            Some(&path),
            apply_input(Some("replacement-secret"), None),
            &store,
            |_candidate| bail!("injected commit failure"),
        )
        .is_err());

        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "{ malformed secret-value"
        );
        assert_eq!(store.0.borrow().operations, ["load", "save", "delete"]);
    }

    #[test]
    fn malformed_provider_values_fail_before_store_access() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        let store = FakeStore::default();
        let mut input = apply_input(Some("secret-value"), None);
        input.stt.base_url = "file:///tmp/provider".into();
        let error = Config::apply(Some(&path), input, &store).unwrap_err();

        assert!(error.to_string().contains("stt.base_url"));
        assert!(!error.to_string().contains("secret-value"));
        assert!(store.0.borrow().operations.is_empty());
        assert!(!path.exists());
    }

    #[test]
    fn removals_preserve_sibling_and_delete_when_both_absent() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        let store = FakeStore::default();
        store.0.borrow_mut().credentials = Some(ManagedKeyringCredentials {
            stt: Some("stt-secret".into()),
            llm: Some("llm-secret".into()),
        });
        let mut config = Config::default();
        config.stt.as_mut().unwrap().auth = managed_auth();
        config.llm.as_mut().unwrap().auth = managed_auth();
        config.save(Some(&path)).unwrap();
        let mut input = apply_input(None, None);
        input.stt.remove_saved_key = true;
        Config::apply(Some(&path), input, &store).unwrap();
        assert_eq!(
            store
                .0
                .borrow()
                .credentials
                .as_ref()
                .unwrap()
                .llm
                .as_deref(),
            Some("llm-secret")
        );

        store.0.borrow_mut().operations.clear();
        let mut input = apply_input(None, None);
        input.summary.llm.remove_saved_key = true;
        Config::apply(Some(&path), input, &store).unwrap();
        let state = store.0.borrow();
        assert_eq!(state.operations, ["load", "delete"]);
        assert!(state.credentials.is_none());
    }

    #[test]
    fn unavailable_store_fails_replacement_and_removal_without_saving_config() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        let store = FakeStore::default();
        store.0.borrow_mut().unavailable = true;
        assert!(Config::apply(
            Some(&path),
            apply_input(Some("replacement-secret"), None),
            &store
        )
        .is_err());
        assert!(!path.exists());

        let mut config = Config::default();
        config.stt.as_mut().unwrap().auth = managed_auth();
        config.save(Some(&path)).unwrap();
        let original = fs::read_to_string(&path).unwrap();
        let mut input = apply_input(None, None);
        input.stt.remove_saved_key = true;
        assert!(Config::apply(Some(&path), input, &store).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
    }

    #[test]
    fn config_candidate_failure_precedes_store_mutation() {
        let directory = tempfile::tempdir().unwrap();
        let parent_file = directory.path().join("not-a-directory");
        fs::write(&parent_file, "occupied").unwrap();
        let path = parent_file.join("config.json");
        let store = FakeStore::default();

        assert!(Config::apply(
            Some(&path),
            apply_input(Some("replacement-secret"), None),
            &store
        )
        .is_err());
        assert_eq!(store.0.borrow().operations, ["load"]);
        assert!(store.0.borrow().credentials.is_none());
        assert!(!path.exists());
    }

    #[test]
    fn config_commit_failure_rolls_back_store_and_leaves_no_partial_config() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        let store = FakeStore::default();

        let error = Config::apply_with_commit(
            Some(&path),
            apply_input(Some("replacement-secret"), None),
            &store,
            |_candidate| bail!("injected commit failure"),
        )
        .unwrap_err();

        assert!(error.to_string().contains("injected commit failure"));
        assert_eq!(store.0.borrow().operations, ["load", "save", "delete"]);
        assert!(store.0.borrow().credentials.is_none());
        assert!(!path.exists());
    }

    #[test]
    fn config_commit_failure_restores_existing_store_and_config() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        let original = Config::default();
        original.save(Some(&path)).unwrap();
        let original_contents = fs::read_to_string(&path).unwrap();
        let store = FakeStore::default();
        store.0.borrow_mut().credentials = Some(ManagedKeyringCredentials {
            stt: Some("original-secret".into()),
            llm: None,
        });

        assert!(Config::apply_with_commit(
            Some(&path),
            apply_input(Some("replacement-secret"), None),
            &store,
            |_candidate| bail!("injected commit failure"),
        )
        .is_err());

        let state = store.0.borrow();
        assert_eq!(state.operations, ["load", "save", "save"]);
        assert_eq!(
            state.credentials.as_ref().unwrap().stt.as_deref(),
            Some("original-secret")
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), original_contents);
    }

    #[test]
    fn status_is_redacted_and_uses_defaults_when_missing() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        let store = FakeStore::default();
        let status = Config::status(Some(&path), &store).unwrap();
        assert!(!status.usable);
        assert!(status.reason.is_some());
        assert!(status.summary_enabled);
        assert_eq!(status.stt.model, "whisper-1");
        assert_eq!(status.llm.model, "gpt-4o-mini");
        assert!(store.0.borrow().operations.is_empty());
        let serialized = serde_json::to_string(&status).unwrap();
        assert!(!serialized.contains("api_key"));
        assert!(!serialized.contains("token"));
        assert!(!serialized.contains("secret"));
    }

    #[test]
    fn invalid_status_advertises_safe_backup_repair() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        fs::write(&path, "not valid JSON").unwrap();

        let status = Config::status(Some(&path), &FakeStore::default()).unwrap();

        assert_eq!(status.reason.as_deref(), Some("invalid_config"));
        assert!(status.invalid_config_will_be_backed_up);
    }

    #[test]
    fn status_distinguishes_managed_plain_and_custom_auth() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        let store = FakeStore::default();
        store.0.borrow_mut().credentials = Some(ManagedKeyringCredentials {
            stt: Some("managed-secret".into()),
            llm: None,
        });
        let mut config = Config::default();
        config.summary_enabled = false;
        config.stt.as_mut().unwrap().auth = managed_auth();
        let status = status_for_config_with_env(path.clone(), config, &store, |_| false).unwrap();
        assert_eq!(status.stt.auth_source, "managed_keyring");
        assert_eq!(status.stt.auth_provenance, "managed");
        assert_eq!(status.stt.managed_credential_configured, Some(true));

        let mut config = Config::default();
        config.summary_enabled = false;
        config.stt.as_mut().unwrap().auth = AuthConfig::BearerPlain {
            token: "plain-secret".into(),
        };
        let status = status_for_config_with_env(path.clone(), config, &store, |_| false).unwrap();
        assert_eq!(status.stt.auth_source, "plaintext");
        assert_eq!(status.stt.managed_credential_configured, None);

        let mut config = Config::default();
        config.summary_enabled = false;
        config.stt.as_mut().unwrap().auth = AuthConfig::BearerKeyring {
            service: "Custom Service".into(),
            user: "Custom User".into(),
        };
        store.0.borrow_mut().keyring.insert(
            ("Custom Service".into(), "Custom User".into()),
            "custom-secret".into(),
        );
        let status = status_for_config_with_env(path, config, &store, |_| false).unwrap();
        assert_eq!(status.stt.auth_source, "custom_keyring");
        assert_eq!(status.stt.auth_provenance, "configured");
        let serialized = serde_json::to_string(&status).unwrap();
        assert!(!serialized.contains("managed-secret"));
        assert!(!serialized.contains("plain-secret"));
        assert!(!serialized.contains("custom-secret"));
    }

    #[test]
    fn interactive_setup_preserves_custom_auth_without_replacement() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        let mut config = Config::default();
        config.stt.as_mut().unwrap().auth = AuthConfig::Header {
            header_name: "X-API-Key".into(),
            value_env: "CUSTOM_STT_KEY".into(),
        };
        config.save(Some(&path)).unwrap();

        setup_provider(Some(&path), "stt", config, String::new()).unwrap();

        let saved = Config::load(Some(&path)).unwrap();
        assert!(matches!(
            saved.stt.unwrap().auth,
            AuthConfig::Header {
                ref header_name,
                ref value_env
            } if header_name == "X-API-Key" && value_env == "CUSTOM_STT_KEY"
        ));
    }

    #[test]
    fn status_reports_unavailable_store_without_exposing_credentials() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        let mut config = Config::default();
        config.stt.as_mut().unwrap().auth = managed_auth();
        config.llm.as_mut().unwrap().auth = managed_auth();
        config.save(Some(&path)).unwrap();
        let store = FakeStore::default();
        store.0.borrow_mut().unavailable = true;

        let status = Config::status(Some(&path), &store).unwrap();
        assert!(!status.usable);
        assert_eq!(
            status.reason.as_deref(),
            Some("credential_store_unavailable")
        );
        assert!(!status.stt.credential_configured);
        assert!(!status.llm.credential_configured);
    }

    #[test]
    fn status_honors_overrides_before_store_access() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        let mut config = Config::default();
        config.stt.as_mut().unwrap().auth = managed_auth();
        config.llm.as_mut().unwrap().auth = managed_auth();
        let store = FakeStore::default();
        store.0.borrow_mut().unavailable = true;

        let status = status_for_config_with_env(path, config, &store, |name| {
            matches!(name, STT_API_KEY_ENV | LLM_API_KEY_ENV)
        })
        .unwrap();

        assert!(status.usable);
        assert!(status.reason.is_none());
        assert!(status.stt.credential_configured);
        assert_eq!(status.stt.managed_credential_configured, None);
        assert_eq!(status.stt.auth_source, "environment");
        assert_eq!(status.stt.auth_provenance, "runtime_override");
        assert!(status.llm.credential_configured);
        assert!(store.0.borrow().operations.is_empty());
    }

    #[test]
    fn status_override_semantics_match_runtime_auth_variants() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        let mut config = Config::default();
        config.summary_enabled = false;
        config.stt.as_mut().unwrap().auth = AuthConfig::Bearer {
            token_env: "CUSTOM_STT_KEY".into(),
        };
        let store = FakeStore::default();

        let status = status_for_config_with_env(path.clone(), config, &store, |name| {
            name == STT_API_KEY_ENV
        })
        .unwrap();
        assert!(!status.usable);
        assert_eq!(status.stt.auth_source, "environment");
        assert_eq!(status.stt.auth_provenance, "configured");

        let mut config = Config::default();
        config.summary_enabled = false;
        config.stt.as_mut().unwrap().auth = AuthConfig::Header {
            header_name: "X-API-Key".into(),
            value_env: "CUSTOM_STT_KEY".into(),
        };
        let status =
            status_for_config_with_env(path, config, &store, |name| name == STT_API_KEY_ENV)
                .unwrap();
        assert!(!status.usable);
        assert_eq!(status.stt.auth_provenance, "custom_header");
        assert!(store.0.borrow().operations.is_empty());
    }

    #[test]
    fn status_skips_disabled_summary_keyring() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        let mut config = Config::default();
        config.summary_enabled = false;
        config.stt.as_mut().unwrap().auth = AuthConfig::None;
        config.llm.as_mut().unwrap().auth = managed_auth();
        let store = FakeStore::default();
        store.0.borrow_mut().unavailable = true;

        let status = status_for_config_with_env(path, config, &store, |_| false).unwrap();

        assert!(status.usable);
        assert!(status.reason.is_none());
        assert!(store.0.borrow().operations.is_empty());
    }

    #[test]
    fn status_preserves_legacy_meetlite_keyring_references() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        let mut config = Config::default();
        config.summary_enabled = false;
        config.stt.as_mut().unwrap().auth = AuthConfig::BearerKeyring {
            service: KEYRING_SERVICE.into(),
            user: LEGACY_STT_USER.into(),
        };
        let store = FakeStore::default();
        store.0.borrow_mut().keyring.insert(
            (KEYRING_SERVICE.into(), LEGACY_STT_USER.into()),
            "legacy-secret".into(),
        );

        let status = status_for_config_with_env(path, config, &store, |_| false).unwrap();

        assert!(status.usable);
        assert!(status.stt.credential_configured);
        assert_eq!(status.stt.auth_source, "legacy_keyring");
        assert_eq!(status.stt.auth_provenance, "legacy_meetlite");
        assert_eq!(store.0.borrow().operations, ["load_keyring"]);
        assert!(!serde_json::to_string(&status)
            .unwrap()
            .contains("legacy-secret"));
    }

    #[test]
    fn persisted_config_without_summary_toggle_defaults_to_enabled() {
        let value = serde_json::to_value(Config::default()).unwrap();
        let mut object = value.as_object().unwrap().clone();
        object.remove("summary_enabled");
        let config: Config = serde_json::from_value(object.into()).unwrap();
        assert!(config.summary_enabled);
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
    fn managed_keychain_credentials_keep_provider_tokens_separate() {
        let mut credentials = ManagedKeyringCredentials::default();
        credentials.set("stt", "stt-token".into());
        credentials.set("llm", "llm-token".into());

        assert_eq!(credentials.token("stt").as_deref(), Some("stt-token"));
        assert_eq!(credentials.token("llm").as_deref(), Some("llm-token"));
    }

    #[test]
    fn legacy_keyring_contract_maps_both_head_locations() {
        assert!(is_legacy_meetlite_keyring(KEYRING_SERVICE, LEGACY_STT_USER));
        assert!(is_legacy_meetlite_keyring(
            LEGACY_KEYRING_SERVICE,
            "stt-api-key"
        ));
        assert_eq!(
            legacy_keyring_users("llm"),
            (LEGACY_LLM_USER, "llm-api-key")
        );
    }

    #[test]
    fn rejects_invalid_recording_rate() {
        let mut config = config_with_stt(AuthConfig::None);
        config.recording.sample_rate = 44_100;

        assert!(config.validate().unwrap_err().to_string().contains("48000"));
    }

    #[test]
    fn accepts_and_drops_legacy_recording_gains() {
        let mut value = serde_json::to_value(Config::default()).unwrap();
        value["recording"]["microphone_gain"] = serde_json::json!(1.0);
        value["recording"]["system_gain"] = serde_json::json!(0.8);

        let config: Config = serde_json::from_value(value).unwrap();
        let saved = serde_json::to_value(config).unwrap();

        assert!(saved["recording"].get("microphone_gain").is_none());
        assert!(saved["recording"].get("system_gain").is_none());
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
