use std::sync::Arc;

use async_trait::async_trait;
use threema_gateway_bot::{
    commands::{CommandStyle, Commands},
    server::handler::{
        Action, CommandType, HandlerError, HandlerResult, MessageContext, MessageHandler, Response,
        TypingHandle,
    },
};

use crate::api::GfroerliClient;

/// Threema bot handler for the Gfrörli water temperature service.
pub struct GfroerliHandler {
    client: Arc<GfroerliClient>,
}

impl GfroerliHandler {
    pub fn new(client: Arc<GfroerliClient>) -> Self {
        Self { client }
    }

    /// Handle `/sensors`: list all available sensors.
    async fn handle_sensors(&self, typing: &TypingHandle) -> HandlerResult<Action> {
        // Start sending typing indicator
        typing.send();

        let text = self
            .client
            .format_sensor_list()
            .await
            .map_err(HandlerError::from)?;
        Ok(Action::Respond(vec![Response::text(text)]))
    }

    /// Handle `/temp <query>`: look up a sensor by name or ID and show its temperature.
    async fn handle_temp(&self, args: &str, typing: &TypingHandle) -> HandlerResult<Action> {
        // Validate query
        let query = args.trim();
        if query.is_empty() {
            return Ok(Action::Respond(vec![Response::text(
                "Please specify a sensor name or ID.\n\nExample: /temp Aare\nExample: /temp 1\n\nUse /sensors to list all available sensors.",
            )]));
        }

        // Start sending typing indicator
        typing.send();

        // Find matching sensors
        let matches = self
            .client
            .find_sensors(query)
            .await
            .map_err(HandlerError::from)?;

        // Respond
        let text = match matches.len() {
            0 => format!(
                "No sensor found matching \"{query}\".\n\nUse /sensors to list all available sensors."
            ),
            1 => matches[0].format_temperature(),
            _ => {
                let mut msg = format!("Multiple sensors match \"{query}\":\n\n");
                for sensor in &matches {
                    msg.push_str(&sensor.format_list_entry());
                    msg.push('\n');
                }
                msg.push_str("\nPlease be more specific or use the sensor ID (e.g. /temp 1).");
                msg
            }
        };
        Ok(Action::Respond(vec![Response::text(text)]))
    }

    /// Handle `/info`: show information about the Gfrörli project.
    fn handle_info() -> Action {
        Action::Respond(vec![Response::text(
            "Gfrörli is a community project that measures water temperatures in Swiss water bodies.\n\n\
             Website: https://gfrör.li/\n\n\
             This bot allows you to quickly check current water temperatures directly from your phone. \
             Use /sensors to see all available measurement stations, or /temp to get the latest reading \
             for a specific sensor.",
        )])
    }
}

#[async_trait]
impl MessageHandler for GfroerliHandler {
    fn description(&self) -> Option<&str> {
        Some("Gfrörli Bot: Check water temperatures in Swiss water bodies.")
    }

    fn commands() -> Commands {
        Commands::new()
            .style(CommandStyle::Slash)
            .register("sensors", "List all available sensors")
            .register("temp", "Get current temperature (e.g. /temp Aare)")
            .register("info", "About the Gfrörli project")
    }

    async fn handle_text(
        &self,
        _ctx: &MessageContext,
        _text: &str,
        _typing: &TypingHandle,
    ) -> HandlerResult<Action> {
        Ok(Action::ShowHelp {
            prelude: Some("I didn't understand that. Here are the available commands:".into()),
        })
    }

    async fn handle_command(
        &self,
        _ctx: &MessageContext,
        command: &str,
        args: &str,
        _command_type: CommandType,
        typing: &TypingHandle,
    ) -> HandlerResult<Action> {
        match command {
            "sensors" => self.handle_sensors(typing).await,
            "temp" => self.handle_temp(args, typing).await,
            "info" => Ok(Self::handle_info()),
            _ => Ok(Action::ShowHelp { prelude: None }),
        }
    }
}
