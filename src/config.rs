use secrecy::SecretString;
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    net::SocketAddr,
    path::{Path, PathBuf},
    time::Duration,
};

use crate::ids::ChatId;

#[derive(Debug, Clone, Deserialize)]
pub struct AppConfig {
    pub telegram: TelegramConfig,
    #[serde(default)]
    pub prompt: PromptConfig,
    pub storage: StorageConfig,
    pub docker: DockerConfig,
    pub codex: CodexConfig,
    pub broker: Option<BrokerConfig>,
    #[serde(default)]
    pub limits: LimitsConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TelegramConfig {
    pub bot_token_env: String,
    pub bot_username: String,
    #[serde(default)]
    pub allowed_chat_ids: Vec<ChatId>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PromptConfig {
    #[serde(default = "default_system_prompt")]
    pub system_prompt: String,
}

impl Default for PromptConfig {
    fn default() -> Self {
        Self {
            system_prompt: default_system_prompt(),
        }
    }
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
    #[serde(default = "default_codex_auth_mode")]
    pub auth_mode: CodexAuthMode,
    #[serde(default)]
    pub auth_host_path: Option<PathBuf>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodexAuthMode {
    BrokerApiKey,
    ChatgptOauth,
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
    #[serde(default = "default_broker_max_requests_per_window")]
    pub broker_max_requests_per_window: usize,
    #[serde(default = "default_broker_rate_limit_window_secs")]
    pub broker_rate_limit_window_secs: u64,
    #[serde(default = "default_broker_max_concurrent_requests")]
    pub broker_max_concurrent_requests: usize,
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
            broker_max_requests_per_window: default_broker_max_requests_per_window(),
            broker_rate_limit_window_secs: default_broker_rate_limit_window_secs(),
            broker_max_concurrent_requests: default_broker_max_concurrent_requests(),
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
        require_non_empty("prompt.system_prompt", &self.prompt.system_prompt)?;
        require_non_empty("docker.image", &self.docker.image)?;
        require_non_empty("docker.network", &self.docker.network)?;
        require_non_empty(
            "docker.workspace_volume_prefix",
            &self.docker.workspace_volume_prefix,
        )?;
        require_non_empty("codex.command", &self.codex.command)?;
        require_non_empty("codex.model", &self.codex.model)?;
        match self.codex.auth_mode {
            CodexAuthMode::BrokerApiKey => {
                let broker = self.broker.as_ref().ok_or(ConfigError::InvalidValue {
                    field: "broker",
                    reason: "is required when codex.auth_mode is broker_api_key",
                })?;
                validate_broker_config(broker)?;
            }
            CodexAuthMode::ChatgptOauth => {
                let Some(auth_host_path) = &self.codex.auth_host_path else {
                    return Err(ConfigError::InvalidValue {
                        field: "codex.auth_host_path",
                        reason: "is required when codex.auth_mode is chatgpt_oauth",
                    });
                };
                if path_is_empty_or_whitespace(auth_host_path) {
                    return Err(ConfigError::InvalidValue {
                        field: "codex.auth_host_path",
                        reason: "must not be empty",
                    });
                }
                if let Some(broker) = &self.broker {
                    validate_broker_config(broker)?;
                }
            }
        }

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
        if self.limits.broker_max_requests_per_window == 0 {
            return Err(ConfigError::InvalidValue {
                field: "limits.broker_max_requests_per_window",
                reason: "must be greater than zero",
            });
        }
        require_positive_u64(
            "limits.broker_rate_limit_window_secs",
            self.limits.broker_rate_limit_window_secs,
        )?;
        if self.limits.broker_max_concurrent_requests == 0 {
            return Err(ConfigError::InvalidValue {
                field: "limits.broker_max_concurrent_requests",
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

    pub fn broker_config(&self) -> Result<&BrokerConfig, ConfigError> {
        self.broker.as_ref().ok_or(ConfigError::InvalidValue {
            field: "broker",
            reason: "is required when codex.auth_mode is broker_api_key",
        })
    }

    pub fn codex_auth_host_path(&self) -> Result<&Path, ConfigError> {
        self.codex
            .auth_host_path
            .as_deref()
            .ok_or(ConfigError::InvalidValue {
                field: "codex.auth_host_path",
                reason: "is required when codex.auth_mode is chatgpt_oauth",
            })
    }
}

fn validate_broker_config(broker: &BrokerConfig) -> Result<(), ConfigError> {
    require_non_empty("broker.public_base_url", &broker.public_base_url)?;
    require_non_empty("broker.upstream_base_url", &broker.upstream_base_url)?;
    require_non_empty("broker.upstream_api_key_env", &broker.upstream_api_key_env)
}

fn path_is_empty_or_whitespace(path: &Path) -> bool {
    path.as_os_str().is_empty() || path.to_string_lossy().trim().is_empty()
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

pub fn broker_limits(config: &AppConfig) -> crate::broker::BrokerLimits {
    crate::broker::BrokerLimits {
        max_requests_per_window: config.limits.broker_max_requests_per_window,
        request_window: Duration::from_secs(config.limits.broker_rate_limit_window_secs),
        max_concurrent_requests: config.limits.broker_max_concurrent_requests,
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

fn default_codex_auth_mode() -> CodexAuthMode {
    CodexAuthMode::BrokerApiKey
}

fn default_system_prompt() -> String {
    "You are the Telegram group assistant for this chat.\nRespond only to the triggering message. Use recent chat and memory as context.".to_owned()
}

fn default_broker_max_requests_per_window() -> usize {
    120
}

fn default_broker_rate_limit_window_secs() -> u64 {
    60
}

fn default_broker_max_concurrent_requests() -> usize {
    4
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
            args = ["exec", "--sandbox", "danger-full-access", "--skip-git-repo-check"]
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
    fn from_toml_str_should_apply_default_system_prompt() {
        let config = AppConfig::from_toml_str(valid_config()).expect("config should parse");

        assert!(
            config
                .prompt
                .system_prompt
                .contains("Telegram group assistant")
        );
    }

    #[test]
    fn from_toml_str_should_parse_configured_system_prompt() {
        let raw = valid_config().replace(
            "[storage]",
            "[prompt]\nsystem_prompt = \"Custom system prompt.\"\n\n[storage]",
        );

        let config = AppConfig::from_toml_str(&raw).expect("config should parse");

        assert_eq!(config.prompt.system_prompt, "Custom system prompt.");
    }

    #[test]
    fn from_toml_str_should_reject_empty_system_prompt() {
        let raw = valid_config().replace(
            "[storage]",
            "[prompt]\nsystem_prompt = \"   \"\n\n[storage]",
        );

        let err = AppConfig::from_toml_str(&raw).expect_err("config should be invalid");

        assert_eq!(
            err.to_string(),
            "config field `prompt.system_prompt` is invalid: must not be empty"
        );
    }

    #[test]
    fn from_toml_str_should_parse_allowed_chat_ids() {
        let raw = valid_config().replace(
            "bot_username = \"telellm_bot\"",
            "bot_username = \"telellm_bot\"\nallowed_chat_ids = [-10012345, 42]",
        );

        let config = AppConfig::from_toml_str(&raw).expect("config should parse");

        assert_eq!(
            config.telegram.allowed_chat_ids,
            vec![ChatId(-10012345), ChatId(42)]
        );
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
    fn from_toml_str_should_default_to_broker_api_key_auth() {
        let config = AppConfig::from_toml_str(valid_config()).expect("config should parse");

        assert_eq!(config.codex.auth_mode, CodexAuthMode::BrokerApiKey);
    }

    #[test]
    fn from_toml_str_should_parse_chatgpt_oauth_without_broker_config() {
        let raw = r#"
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
            args = ["exec", "--sandbox", "danger-full-access", "--skip-git-repo-check"]
            model = "gpt-5-codex"
            auth_mode = "chatgpt_oauth"
            auth_host_path = "/home/user/.codex/auth.json"
        "#;

        let config = AppConfig::from_toml_str(raw).expect("config should parse");

        assert_eq!(config.codex.auth_mode, CodexAuthMode::ChatgptOauth);
        assert!(config.broker.is_none());
    }

    #[test]
    fn from_toml_str_should_reject_chatgpt_oauth_without_auth_host_path() {
        let raw = valid_config()
            .replace(
                "model = \"gpt-5-codex\"",
                "model = \"gpt-5-codex\"\n            auth_mode = \"chatgpt_oauth\"",
            )
            .replace(
                r#"
            [broker]
            listen = "127.0.0.1:8189"
            public_base_url = "http://host.docker.internal:8189/v1"
            upstream_base_url = "https://api.openai.com/v1"
            upstream_api_key_env = "OPENAI_API_KEY"
        "#,
                "",
            );

        let err = AppConfig::from_toml_str(&raw).expect_err("config should be invalid");

        assert_eq!(
            err.to_string(),
            "config field `codex.auth_host_path` is invalid: is required when codex.auth_mode is chatgpt_oauth"
        );
    }

    #[test]
    fn from_toml_str_should_reject_chatgpt_oauth_empty_auth_host_path() {
        let raw = r#"
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
            model = "gpt-5-codex"
            auth_mode = "chatgpt_oauth"
            auth_host_path = "   "
        "#;

        let err = AppConfig::from_toml_str(raw).expect_err("config should be invalid");

        assert_eq!(
            err.to_string(),
            "config field `codex.auth_host_path` is invalid: must not be empty"
        );
    }

    #[test]
    fn from_toml_str_should_reject_broker_api_key_without_broker_config() {
        let raw = valid_config().replace(
            r#"
            [broker]
            listen = "127.0.0.1:8189"
            public_base_url = "http://host.docker.internal:8189/v1"
            upstream_base_url = "https://api.openai.com/v1"
            upstream_api_key_env = "OPENAI_API_KEY"
        "#,
            "",
        );

        let err = AppConfig::from_toml_str(&raw).expect_err("config should be invalid");

        assert_eq!(
            err.to_string(),
            "config field `broker` is invalid: is required when codex.auth_mode is broker_api_key"
        );
    }

    #[test]
    fn from_toml_str_should_reject_unknown_codex_auth_mode() {
        let raw = valid_config().replace(
            "model = \"gpt-5-codex\"",
            "model = \"gpt-5-codex\"\n            auth_mode = \"session_cookie\"",
        );

        let err = AppConfig::from_toml_str(&raw).expect_err("config should be invalid");

        assert!(err.to_string().contains("unknown variant `session_cookie`"));
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
