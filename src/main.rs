use std::{env, path::PathBuf, sync::Arc};

use anyhow::{Context, Result};
use gfroerli_bot::{api::GfroerliClient, config::AppConfig, handler::GfroerliHandler};
use threema_gateway_bot::server::BotServer;
use tracing::info;
use tracing_subscriber::{EnvFilter, fmt};

/// Parse the optional `--config <path>` flag from command-line arguments.
fn parse_args() -> Result<Option<PathBuf>> {
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--config" {
            let path = args.next().context("--config requires a path argument")?;
            return Ok(Some(PathBuf::from(path)));
        }
    }
    Ok(None)
}

#[tokio::main]
async fn main() -> Result<()> {
    fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,threema_gateway_bot=debug")),
        )
        .init();

    let config_path = parse_args()?;
    let app_config = AppConfig::load(config_path.as_deref())?;
    let (bot_config, bot_settings, gfroerli_config) = app_config.split();

    info!(
        "Starting Gfrörli bot on {}:{}",
        bot_config.server.host, bot_config.server.port
    );

    let client = Arc::new(GfroerliClient::new(gfroerli_config));
    client
        .validate_api_key()
        .await
        .context("Gfrörli API key validation failed")?;
    let handler = GfroerliHandler::new(client, bot_settings.maintainer_ids);

    BotServer::new(bot_config, handler)?.run().await?;

    Ok(())
}
