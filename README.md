# Gfrörli Threema Bot

A Threema bot for querying water temperatures from the [Gfrörli](https://gfrör.li/) sensor network.

## Commands

- `/sensors` — List all available sensors
- `/temp <query>` — Get the current temperature for a sensor (by name or ID)
- `/stats <query>` — Show stats and charts for a sensor (by name or ID)
- `/sponsors` — List all project sponsors
- `/sponsor <query>` — Show the sponsor for a sensor (by name or ID)
- `/about` — About the Gfrörli project
- `/help` — Show available commands

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
