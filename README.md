# Gfrörli Threema Bot

A Threema bot for querying water temperatures from the [Gfrörli](https://gfrör.li/) sensor network.

## Commands

- `/sensors` — List all available sensors
- `/temp <query>` — Get the current temperature for a sensor (by name or ID)
- `/stats <query>` — Show stats and charts for a sensor (by name or ID)
- `/sponsors` — List all project sponsors
- `/sponsor <query>` — Show the sponsor for a sensor (by name or ID)
- `/alert <query> <temperature>` — Get notified once a sensor's afternoon water temperature reaches the given threshold (°C)
- `/alerts` — List your active alerts
- `/unalert <query>` — Remove an alert (or `/unalert all` to remove every alert)
- `/about` — About the Gfrörli project
- `/help` — Show available commands

Creating an alert stores your Threema ID so the bot can message you; removing the alert (`/unalert
<query>`, or `/unalert all`) deletes it again. Alerts are persisted in a SQLite database (see the
`[database]` section in `config.toml`).

## Alerts

`/alert <sensor> <temperature>` notifies you once a sensor's water is reliably warm enough for
swimming. To avoid spurious alerts from the daily temperature swing, the bot judges each day by the
**afternoon average** (the mean over 12:00–18:00 local) rather than a momentary reading, and applies
some hysteresis:

- Each evening (around 19:00 Europe/Zurich, once the afternoon window has closed) every alert is
  evaluated once.
- You're notified when the afternoon average is at or above your threshold on **two days in a row**.
- After notifying, the alert goes quiet. It resets only once the afternoon average stays clearly
  below your threshold for **three days in a row**, so a cold snap followed by a warm spell will
  alert you again.

You can hold one alert per sensor; sending `/alert` again for the same sensor with a different
temperature updates the threshold (and restarts the evaluation).

## Configuration

Copy `config.toml.example` to `config.toml` and fill in the values. All config values can also be
set via environment variables with the `GFROERLI_BOT__` prefix (e.g.
`GFROERLI_BOT__THREEMA__API_SECRET`).

## Usage

    cargo run -- --config config.toml

The `--config` flag is optional if all values are provided via environment variables.

See `config.toml.example for an example config file.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or
  http://www.apache.org/licenses/LICENSE-2.0)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or
  http://opensource.org/licenses/MIT) at your option.

### Contributing

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall
be dual licensed as above, without any additional terms or conditions.
