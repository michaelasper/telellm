use secrecy::SecretString;
use serde::Deserialize;
use std::{collections::BTreeMap, net::SocketAddr, path::PathBuf, time::Duration};

#[derive(Debug, Clone, Deserialize)]
pub struct AppConfig {
    pub telegram: TelegramConfig,
    pub storage: StorageConfig,
    pub docker: DockerConfig,
    pub codex: CodexConfig,
    pub broker: BrokerConfig,
    #[serde(default)]
    pub limits: LimitsConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TelegramConfig {
    pub bot_token_env: String,
    pub bot_username: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StorageConfig {
    pub sqlite_path: PathBuf,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DockerConfig {
    pub image: String,
    pub network: String,
    pub workspace_volume_prefix: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CodexConfig {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    pub model: String,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BrokerConfig {
    pub listen: SocketAddr,
    pub public_base_url: String,
    pub upstream_base_url: String,
    pub upstream_api_key_env: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LimitsConfig {
    #[serde(default = "default_queue_depth")]
    pub per_group_queue_depth: usize,
    #[serde(default = "default_telegram_chunk_chars")]
    pub telegram_chunk_chars: usize,
    #[serde(default = "default_recent_buffer_messages")]
    pub recent_buffer_messages: usize,
    #[serde(default = "default_codex_first_byte_timeout_secs")]
    pub codex_first_byte_timeout_secs: u64,
    #[serde(default = "default_codex_inactivity_secs")]
    pub codex_inactivity_secs: u64,
    #[serde(default = "default_codex_max_turn_secs")]
    pub codex_max_turn_secs: u64,
    #[serde(default = "default_codex_max_output_bytes")]
    pub codex_max_output_bytes: usize,
}

impl Default for LimitsConfig {
    fn default() -> Self {
        Self {
            per_group_queue_depth: default_queue_depth(),
            telegram_chunk_chars: default_telegram_chunk_chars(),
            recent_buffer_messages: default_recent_buffer_messages(),
            codex_first_byte_timeout_secs: default_codex_first_byte_timeout_secs(),
            codex_inactivity_secs: default_codex_inactivity_secs(),
            codex_max_turn_secs: default_codex_max_turn_secs(),
            codex_max_output_bytes: default_codex_max_output_bytes(),
        }
    }
}

impl AppConfig {
    pub fn from_path(path: &std::path::Path) -> Result<Self, ConfigError> {
        let raw = std::fs::read_to_string(path)?;
        Self::from_toml_str(&raw)
    }

    pub fn from_toml_str(raw: &str) -> Result<Self, ConfigError> {
        let config: Self = toml::from_str(raw)?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        require_non_empty("telegram.bot_token_env", &self.telegram.bot_token_env)?;
        require_non_empty("telegram.bot_username", &self.telegram.bot_username)?;
        require_non_empty("docker.image", &self.docker.image)?;
        require_non_empty("docker.network", &self.docker.network)?;
        require_non_empty(
            "docker.workspace_volume_prefix",
            &self.docker.workspace_volume_prefix,
        )?;
        require_non_empty("codex.command", &self.codex.command)?;
        require_non_empty("codex.model", &self.codex.model)?;
        require_non_empty("broker.public_base_url", &self.broker.public_base_url)?;
        require_non_empty("broker.upstream_base_url", &self.broker.upstream_base_url)?;
        require_non_empty(
            "broker.upstream_api_key_env",
            &self.broker.upstream_api_key_env,
        )?;

        if self.limits.per_group_queue_depth == 0 {
            return Err(ConfigError::InvalidValue {
                field: "limits.per_group_queue_depth",
                reason: "must be greater than zero",
            });
        }

        if self.limits.telegram_chunk_chars < 256 {
            return Err(ConfigError::InvalidValue {
                field: "limits.telegram_chunk_chars",
                reason: "must be at least 256",
            });
        }

        require_positive_u64(
            "limits.codex_first_byte_timeout_secs",
            self.limits.codex_first_byte_timeout_secs,
        )?;
        require_positive_u64(
            "limits.codex_inactivity_secs",
            self.limits.codex_inactivity_secs,
        )?;
        require_positive_u64(
            "limits.codex_max_turn_secs",
            self.limits.codex_max_turn_secs,
        )?;
        if self.limits.codex_max_output_bytes == 0 {
            return Err(ConfigError::InvalidValue {
                field: "limits.codex_max_output_bytes",
                reason: "must be greater than zero",
            });
        }

        Ok(())
    }

    pub fn telegram_token_from_env(&self) -> Result<SecretString, ConfigError> {
        let value = std::env::var(&self.telegram.bot_token_env).map_err(|_| {
            ConfigError::MissingSecretEnv {
                env: self.telegram.bot_token_env.clone(),
            }
        })?;
        Ok(SecretString::from(value))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("failed to read config file: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to parse config TOML: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("config field `{field}` is invalid: {reason}")]
    InvalidValue {
        field: &'static str,
        reason: &'static str,
    },
    #[error("required secret environment variable `{env}` is not set")]
    MissingSecretEnv { env: String },
}

fn require_non_empty(field: &'static str, value: &str) -> Result<(), ConfigError> {
    if value.trim().is_empty() {
        return Err(ConfigError::InvalidValue {
            field,
            reason: "must not be empty",
        });
    }
    Ok(())
}

fn require_positive_u64(field: &'static str, value: u64) -> Result<(), ConfigError> {
    if value == 0 {
        return Err(ConfigError::InvalidValue {
            field,
            reason: "must be greater than zero",
        });
    }
    Ok(())
}

pub fn codex_inactivity_timeout(config: &AppConfig) -> Duration {
    Duration::from_secs(config.limits.codex_inactivity_secs)
}

pub fn codex_read_policy(config: &AppConfig) -> crate::codex::pty::PtyReadPolicy {
    crate::codex::pty::PtyReadPolicy {
        first_byte_timeout: Duration::from_secs(config.limits.codex_first_byte_timeout_secs),
        inactivity_timeout: Duration::from_secs(config.limits.codex_inactivity_secs),
        max_turn_timeout: Duration::from_secs(config.limits.codex_max_turn_secs),
        max_output_bytes: config.limits.codex_max_output_bytes,
    }
}

fn default_queue_depth() -> usize {
    16
}

fn default_telegram_chunk_chars() -> usize {
    3900
}

fn default_recent_buffer_messages() -> usize {
    200
}

fn default_codex_first_byte_timeout_secs() -> u64 {
    300
}

fn default_codex_inactivity_secs() -> u64 {
    600
}

fn default_codex_max_turn_secs() -> u64 {
    900
}

fn default_codex_max_output_bytes() -> usize {
    512 * 1024
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_config() -> &'static str {
        r#"
            [telegram]
            bot_token_env = "TELEGRAM_BOT_TOKEN"
            bot_username = "telellm_bot"

            [storage]
            sqlite_path = "data/telellm.sqlite"

            [docker]
            image = "telellm-sandbox:local"
            network = "telellm_public"
            workspace_volume_prefix = "telellm_workspace"

            [codex]
            command = "codex"
            args = ["--sandbox", "danger-full-access", "--ask-for-approval", "never"]
            model = "gpt-5-codex"

            [broker]
            listen = "127.0.0.1:8189"
            public_base_url = "http://host.docker.internal:8189/v1"
            upstream_base_url = "https://api.openai.com/v1"
            upstream_api_key_env = "OPENAI_API_KEY"
        "#
    }

    #[test]
    fn from_toml_str_should_parse_valid_config() {
        let config = AppConfig::from_toml_str(valid_config()).expect("config should parse");

        assert_eq!(config.telegram.bot_username, "telellm_bot");
    }

    #[test]
    fn from_toml_str_should_reject_empty_bot_username() {
        let raw = valid_config().replace("telellm_bot", "");

        let err = AppConfig::from_toml_str(&raw).expect_err("config should be invalid");

        assert_eq!(
            err.to_string(),
            "config field `telegram.bot_username` is invalid: must not be empty"
        );
    }

    #[test]
    fn from_toml_str_should_apply_default_limits() {
        let config = AppConfig::from_toml_str(valid_config()).expect("config should parse");

        assert_eq!(config.limits.per_group_queue_depth, 16);
    }

    #[test]
    fn from_toml_str_should_reject_zero_codex_inactivity_secs() {
        let raw = format!("{}\n[limits]\ncodex_inactivity_secs = 0\n", valid_config());

        let err = AppConfig::from_toml_str(&raw).expect_err("config should be invalid");

        assert_eq!(
            err.to_string(),
            "config field `limits.codex_inactivity_secs` is invalid: must be greater than zero"
        );
    }

    #[test]
    fn codex_read_policy_should_use_configured_limits() {
        let raw = format!(
            "{}\n[limits]\n\
            codex_first_byte_timeout_secs = 11\n\
            codex_inactivity_secs = 7\n\
            codex_max_turn_secs = 29\n\
            codex_max_output_bytes = 12345\n",
            valid_config()
        );
        let config = AppConfig::from_toml_str(&raw).expect("config should parse");

        let policy = codex_read_policy(&config);

        assert_eq!(policy.first_byte_timeout, Duration::from_secs(11));
        assert_eq!(policy.inactivity_timeout, Duration::from_secs(7));
        assert_eq!(policy.max_turn_timeout, Duration::from_secs(29));
        assert_eq!(policy.max_output_bytes, 12345);
    }
}
