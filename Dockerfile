# WHAT: builds the Automotrix bot into a small runtime image.
# WHY:  so the whole demo - database, mail sink, bot - comes up with one
#       `docker compose up`, the same way on any machine.
# HOW:  a Rust build stage compiles the server and the seed loader; the runtime
#       stage is plain Debian with only CA certificates added. Every TLS path in
#       the app (Postgres, SMTP, the Claude API, the HTTPS server) uses rustls, so
#       there is no OpenSSL to install or patch.
#
#       On start the container runs `load` (which applies the migrations and the
#       seed data - both idempotent) and then execs the server.

FROM rust:1.98-slim-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates crates
COPY migrations migrations
RUN cargo build --release --bin server --bin load

FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=build /src/target/release/server /src/target/release/load /usr/local/bin/
COPY crates/automotrix/templates crates/automotrix/templates
COPY config config
COPY migrations migrations
COPY seed seed
COPY web web
COPY cars cars
ENV AUTOMOTRIX_ROOT=/app \
    RUST_LOG=info,sqlx=warn,tower_http=warn
EXPOSE 8443
CMD ["sh", "-c", "load && exec server"]
