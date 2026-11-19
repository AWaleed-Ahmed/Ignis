//! LLM endpoint configuration, read only from the connector's own environment.

use std::net::IpAddr;

use url::Url;

pub const DEFAULT_MAX_CALLS_PER_JOB: u32 = 3;
pub const DEFAULT_MAX_CALLS_PER_DAY: u32 = 20;

/// Customer-owned model endpoint. Any OpenAI-compatible chat-completions API
/// works: hosted providers with a key, or a keyless local server (Ollama, vLLM,
/// llama.cpp).
#[derive(Clone)]
pub struct LlmConfig {
    pub base_url: Url,
    pub model: String,
    pub api_key: Option<String>,
    pub provider: String,
    pub local: bool,
    pub max_calls_per_job: u32,
    pub max_calls_per_day: u32,
}

impl std::fmt::Debug for LlmConfig {
    // Never print the key.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LlmConfig")
            .field("base_url", &self.base_url.as_str())
            .field("model", &self.model)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("provider", &self.provider)
            .field("local", &self.local)
            .field("max_calls_per_job", &self.max_calls_per_job)
            .field("max_calls_per_day", &self.max_calls_per_day)
            .finish()
    }
}

/// True for loopback, private-network and in-cluster hosts.
pub fn is_local_host(url: &Url) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = host.parse::<IpAddr>() {
        return match ip {
            IpAddr::V4(v4) => v4.is_loopback() || v4.is_private() || v4.is_link_local(),
            IpAddr::V6(v6) => v6.is_loopback() || (v6.segments()[0] & 0xfe00) == 0xfc00,
        };
    }
    host == "localhost"
        || host.ends_with(".localhost")
        || host == "host.docker.internal"
        || host.ends_with(".svc")
        || host.ends_with(".svc.cluster.local")
}

impl LlmConfig {
    /// `Ok(None)` when no model endpoint is configured.
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> anyhow::Result<Option<Self>> {
        let value = |name: &str| {
            get(name)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        let Some(raw_url) = value("RAPHAEL_CONNECTOR_LLM_BASE_URL") else {
            return Ok(None);
        };
        let base_url = Url::parse(raw_url.trim_end_matches('/'))
            .map_err(|_| anyhow::anyhow!("RAPHAEL_CONNECTOR_LLM_BASE_URL must be a valid URL"))?;
        if !["http", "https"].contains(&base_url.scheme()) {
            return Err(anyhow::anyhow!(
                "RAPHAEL_CONNECTOR_LLM_BASE_URL must use http:// or https://"
            ));
        }
        let local = is_local_host(&base_url);
        if base_url.scheme() == "http" && !local {
            return Err(anyhow::anyhow!(
                "RAPHAEL_CONNECTOR_LLM_BASE_URL must use https:// for non-local endpoints"
            ));
        }
        let model = value("RAPHAEL_CONNECTOR_LLM_MODEL").ok_or_else(|| {
            anyhow::anyhow!("RAPHAEL_CONNECTOR_LLM_MODEL is required when an LLM endpoint is set")
        })?;
        let api_key = value("RAPHAEL_CONNECTOR_LLM_API_KEY");
        if api_key.is_none() && !local {
            return Err(anyhow::anyhow!(
                "RAPHAEL_CONNECTOR_LLM_API_KEY is required for non-local endpoints"
            ));
        }
        let cap = |name: &str, default: u32, max: u32| -> anyhow::Result<u32> {
            match value(name) {
                None => Ok(default),
                Some(raw) => match raw.parse::<u32>() {
                    Ok(n) if (1..=max).contains(&n) => Ok(n),
                    _ => Err(anyhow::anyhow!("{name} must be an integer from 1 to {max}")),
                },
            }
        };
        let provider = value("RAPHAEL_CONNECTOR_LLM_PROVIDER")
            .unwrap_or_else(|| if local { "local" } else { "openai_compatible" }.to_string());
        Ok(Some(Self {
            base_url,
            model,
            api_key,
            provider,
            local,
            max_calls_per_job: cap(
                "RAPHAEL_CONNECTOR_LLM_MAX_CALLS_PER_JOB",
                DEFAULT_MAX_CALLS_PER_JOB,
                3,
            )?,
            max_calls_per_day: cap(
                "RAPHAEL_CONNECTOR_LLM_MAX_CALLS_PER_DAY",
                DEFAULT_MAX_CALLS_PER_DAY,
                10_000,
            )?,
        }))
    }

    pub fn from_env() -> anyhow::Result<Option<Self>> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn load(pairs: &[(&str, &str)]) -> anyhow::Result<Option<LlmConfig>> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        LlmConfig::from_lookup(|name| map.get(name).cloned())
    }

    #[test]
    fn unset_endpoint_disables_the_feature() {
        assert!(load(&[]).unwrap().is_none());
    }

    #[test]
    fn hosted_endpoint_needs_https_and_a_key() {
        let base = (
            "RAPHAEL_CONNECTOR_LLM_BASE_URL",
            "https://api.openai.com/v1",
        );
        let model = ("RAPHAEL_CONNECTOR_LLM_MODEL", "gpt-x");
        assert!(load(&[base, model]).is_err());
        let config = load(&[base, model, ("RAPHAEL_CONNECTOR_LLM_API_KEY", "sk-test")])
            .unwrap()
            .unwrap();
        assert!(!config.local);
        assert_eq!(config.provider, "openai_compatible");
        assert_eq!(config.max_calls_per_job, 3);
        assert_eq!(config.max_calls_per_day, 20);
        let plain = ("RAPHAEL_CONNECTOR_LLM_BASE_URL", "http://api.openai.com/v1");
        assert!(load(&[plain, model, ("RAPHAEL_CONNECTOR_LLM_API_KEY", "sk-test")]).is_err());
    }

    #[test]
    fn local_endpoint_may_be_keyless_and_plain_http() {
        for url in [
            "http://localhost:11434/v1",
            "http://127.0.0.1:8000/v1",
            "http://10.0.0.5:8000/v1",
            "http://ollama.models.svc.cluster.local:11434/v1",
            "http://[::1]:11434/v1",
        ] {
            let config = load(&[
                ("RAPHAEL_CONNECTOR_LLM_BASE_URL", url),
                ("RAPHAEL_CONNECTOR_LLM_MODEL", "llama3"),
            ])
            .unwrap()
            .unwrap();
            assert!(config.local, "{url}");
            assert!(config.api_key.is_none());
            assert_eq!(config.provider, "local");
        }
    }

    #[test]
    fn model_is_required_and_caps_are_bounded() {
        let base = (
            "RAPHAEL_CONNECTOR_LLM_BASE_URL",
            "http://localhost:11434/v1",
        );
        assert!(load(&[base]).is_err());
        let model = ("RAPHAEL_CONNECTOR_LLM_MODEL", "llama3");
        assert!(load(&[
            base,
            model,
            ("RAPHAEL_CONNECTOR_LLM_MAX_CALLS_PER_JOB", "4")
        ])
        .is_err());
        assert!(load(&[
            base,
            model,
            ("RAPHAEL_CONNECTOR_LLM_MAX_CALLS_PER_DAY", "0")
        ])
        .is_err());
        let config = load(&[
            base,
            model,
            ("RAPHAEL_CONNECTOR_LLM_MAX_CALLS_PER_DAY", "5"),
        ])
        .unwrap()
        .unwrap();
        assert_eq!(config.max_calls_per_day, 5);
    }

    #[test]
    fn debug_output_redacts_the_key() {
        let config = load(&[
            (
                "RAPHAEL_CONNECTOR_LLM_BASE_URL",
                "https://api.example.com/v1",
            ),
            ("RAPHAEL_CONNECTOR_LLM_MODEL", "m"),
            ("RAPHAEL_CONNECTOR_LLM_API_KEY", "sk-very-secret"),
        ])
        .unwrap()
        .unwrap();
        let debug = format!("{config:?}");
        assert!(!debug.contains("sk-very-secret") && debug.contains("<redacted>"));
    }
}
