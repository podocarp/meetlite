use std::env;

use anyhow::{bail, Context, Result};
use reqwest::{
    blocking::RequestBuilder,
    header::{HeaderName, HeaderValue, AUTHORIZATION},
};

use crate::config::{AuthConfig, LLM_API_KEY_ENV, STT_API_KEY_ENV};

#[derive(Clone)]
pub enum Credentials {
    None,
    Bearer(String),
    Header {
        name: HeaderName,
        value: HeaderValue,
    },
}

impl Credentials {
    pub fn for_stt(auth: &AuthConfig) -> Result<Self> {
        Self::resolve(auth, STT_API_KEY_ENV, "stt")
    }

    pub fn for_llm(auth: &AuthConfig) -> Result<Self> {
        Self::resolve(auth, LLM_API_KEY_ENV, "llm")
    }

    pub fn apply(&self, request: RequestBuilder) -> RequestBuilder {
        match self {
            Credentials::None => request,
            Credentials::Bearer(token) => request.header(AUTHORIZATION, format!("Bearer {token}")),
            Credentials::Header { name, value } => request.header(name, value),
        }
    }

    fn resolve(auth: &AuthConfig, override_env: &str, provider: &str) -> Result<Self> {
        match auth {
            AuthConfig::None => Ok(Credentials::None),
            AuthConfig::Bearer { token_env } => {
                Ok(Credentials::Bearer(environment_value(token_env)?))
            }
            AuthConfig::BearerKeyring { .. } | AuthConfig::BearerPlain { .. } => Ok(auth
                .bearer_token(override_env, provider)?
                .map(Credentials::Bearer)
                .unwrap_or(Credentials::None)),
            AuthConfig::Header {
                header_name,
                value_env,
            } => Ok(Credentials::Header {
                name: HeaderName::from_bytes(header_name.as_bytes())
                    .context("configured authentication header name is invalid")?,
                value: HeaderValue::from_str(&environment_value(value_env)?)
                    .context("configured authentication header value is invalid")?,
            }),
        }
    }
}

fn environment_value(name: &str) -> Result<String> {
    let value = env::var(name)
        .with_context(|| format!("required environment variable {name} is not set"))?;
    if value.is_empty() {
        bail!("required environment variable {name} is empty")
    }
    Ok(value)
}
