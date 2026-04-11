use std::path::Path;

use serde::Deserialize;
use threema_gateway_bot::config::{BotConfig, RateLimitingConfig, ServerConfig, ThreemaConfig};

/// Top-level application configuration.
///
/// Embeds the standard [`BotConfig`] sections and adds a custom `[gfroerli]` section for
/// the Gfrörli API credentials.
#[derive(Deserialize)]
pub struct AppConfig {
    server: ServerConfig,
    threema: ThreemaConfig,
    #[serde(default)]
    rate_limiting: RateLimitingConfig,
    pub gfroerli: GfroerliConfig,
}

/// Configuration for the Gfrörli REST API.
#[derive(Deserialize)]
pub struct GfroerliConfig {
    /// Base URL of the Gfrörli API (e.g. `https://api.gfrör.li`).
    pub api_url: String,
    /// Read-only bearer token for API authentication.
    pub api_key: String,
}

/// Environment variable prefix used for configuration overrides.
const ENV_PREFIX: &str = "GFROERLI_BOT";

/// Separator for nested configuration values in environment variables.
const ENV_SEPARATOR: &str = "__";

impl AppConfig {
    /// Load configuration from an optional TOML file with environment variable overrides.
    pub fn load(config_path: Option<&Path>) -> anyhow::Result<Self> {
        let mut builder = config::Config::builder();
        if let Some(path) = config_path {
            builder = builder.add_source(config::File::from(path).required(false));
        }
        let raw = builder
            .add_source(
                config::Environment::with_prefix(ENV_PREFIX)
                    .prefix_separator(ENV_SEPARATOR)
                    .separator(ENV_SEPARATOR)
                    .try_parsing(true),
            )
            .build()?;
        let config: Self = raw.try_deserialize()?;
        Ok(config)
    }

    /// Split into [`BotConfig`] (for [`BotServer`](threema_gateway_bot::server::BotServer)) and [`GfroerliConfig`].
    pub fn split(self) -> (BotConfig, GfroerliConfig) {
        let bot_config = BotConfig {
            server: self.server,
            threema: self.threema,
            rate_limiting: self.rate_limiting,
        };
        (bot_config, self.gfroerli)
    }
}
