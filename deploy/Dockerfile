FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release -p cc-api

# The phone web build of the same Leptos window (shim.js stands in for Tauri), served at /app/.
FROM rust:1-bookworm AS web
ARG TRUNK_VERSION=0.22.2
RUN rustup target add wasm32-unknown-unknown \
    && curl -fsSL "https://github.com/trunk-rs/trunk/releases/download/v${TRUNK_VERSION}/trunk-x86_64-unknown-linux-gnu.tar.gz" \
       | tar -xz -C /usr/local/bin trunk
WORKDIR /src
COPY . .
RUN cd crates/web/ui && trunk build web.html --release=true --dist /web --public-url /app/

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 cc
COPY --from=build /src/target/release/cc-api /src/target/release/cc-admin /src/target/release/cc-mock-model /usr/local/bin/
COPY --from=web /web /srv/web
USER cc
EXPOSE 8080
CMD ["cc-api"]
