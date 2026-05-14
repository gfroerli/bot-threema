# Build
FROM rust:1-slim-trixie AS builder
COPY . /src
RUN apt-get update \
    && apt-get install -y cmake pkg-config libsqlite3-dev \
    && rm -rf /var/lib/apt/lists/*
RUN cd /src && cargo build --release

# Create runtime container
# Note that we need a small init process for PID 1 that forwards signals.
# See https://github.com/Yelp/dumb-init
FROM debian:13-slim
RUN apt-get update && apt-get install -y ca-certificates dumb-init sqlite3 && rm -rf /var/lib/apt/lists/*
COPY --from=builder /src/target/release/gfroerli-bot-threema /usr/local/bin/
RUN groupadd --gid 2345 gfroerli-bot-threema \
    && useradd --no-create-home --uid 2345 --gid 2345 gfroerli-bot-threema \
    && chown gfroerli-bot-threema:gfroerli-bot-threema /usr/local/bin/gfroerli-bot-threema
USER gfroerli-bot-threema
WORKDIR /home/gfroerli-bot-threema
ENTRYPOINT ["/usr/bin/dumb-init", "--"]
CMD [ "gfroerli-bot-threema", "--config", "/etc/gfroerli-bot-threema.toml" ]
