use std::{env, path::PathBuf, sync::Arc};

use anyhow::{Context, Result};
use gfroerli_bot_threema::{
    api::GfroerliClient, config::AppConfig, db::Database, handler::GfroerliHandler, scheduler,
    store::AlertStore,
};
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
    // Set up logging
    fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            EnvFilter::new("info,gfroerli_bot_threema=debug,threema_gateway_bot=debug")
        }))
        .init();

    // Load config
    let config_path = parse_args()?;
    let app_config = AppConfig::load(config_path.as_deref())?;
    let (bot_config, bot_settings, gfroerli_config, database_config) = app_config.split();
    info!(
        "Starting Gfrörli bot on {}:{}",
        bot_config.server.host, bot_config.server.port
    );

    // Open (and migrate) the database, then build the alert store on top of it
    let database = Database::connect(&database_config.path)
        .await
        .context("opening database")?;
    let store = AlertStore::new(&database);
    info!("Active alerts: {}", store.count().await?);

    // Prepare client and handler
    let client = Arc::new(GfroerliClient::new(gfroerli_config));
    client
        .validate_api_key()
        .await
        .context("Gfrörli API key validation failed")?;
    let handler = GfroerliHandler::new(client.clone(), store.clone(), bot_settings.maintainer_ids);

    // Build the server instance
    let server = BotServer::new(bot_config, handler)?;

    // Spawn scheduler background task
    tokio::spawn(scheduler::run(store, client, server.client()));

    // Run bot server
    server.run().await?;

    Ok(())
}
