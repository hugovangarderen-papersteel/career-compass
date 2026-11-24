FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release -p cc-api

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 cc
COPY --from=build /src/target/release/cc-api /src/target/release/cc-admin /usr/local/bin/
USER cc
EXPOSE 8080
CMD ["cc-api"]
