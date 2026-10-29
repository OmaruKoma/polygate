# Multi-stage build for PolyGate.
#
#   docker build -f deploy/Dockerfile -t polygate:local .
#   docker run --rm -p 127.0.0.1:8787:8787 \
#     -e POLYGATE_API_KEY=... -e POLYGATE_MASTER_KEY=... \
#     -v polygate-data:/var/lib/polygate polygate:local
#
# The image listens on 0.0.0.0 inside the container so the port mapping works.
# That does not expose it publicly by itself; publish to 127.0.0.1 or behind a
# TLS reverse proxy.
FROM rust:1-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY src ./src
COPY migrations ./migrations
COPY web ./web
RUN cargo build --release --locked

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates tzdata \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --home /var/lib/polygate --shell /usr/sbin/nologin polygate \
    && install -d -o polygate -g polygate -m 0700 /var/lib/polygate
COPY --from=build /src/target/release/polygate /usr/local/bin/polygate
COPY deploy/container-config.toml /etc/polygate/config.toml
USER polygate
WORKDIR /var/lib/polygate
VOLUME ["/var/lib/polygate"]
EXPOSE 8787
ENTRYPOINT ["/usr/local/bin/polygate", "--config", "/etc/polygate/config.toml", "--data-dir", "/var/lib/polygate"]
CMD ["serve"]
