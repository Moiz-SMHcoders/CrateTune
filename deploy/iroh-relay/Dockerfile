# CrateTune's own iroh relay for device hand-off (see README.md).
#
# Builds the pinned `iroh-relay` binary from crates.io (its own Cargo.lock, `--locked`)
# and runs it as a non-root user on a distroless base. TLS is not handled here: the
# relay serves plain HTTP on the LAN and the existing Caddy proxy terminates TLS.
#
#   docker build -t cratetune/iroh-relay:1.4.1 .

FROM rust:1.99.1-slim-bookworm AS build
ARG IROH_RELAY_VERSION=1.4.1
RUN cargo install iroh-relay --version "=${IROH_RELAY_VERSION}" --locked \
        --features server --root /out \
    && strip /out/bin/iroh-relay

FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=build /out/bin/iroh-relay /usr/local/bin/iroh-relay
# The config is mounted read-only by docker-compose.yml (iroh-relay.toml here).
USER nonroot:nonroot
EXPOSE 3340/tcp
ENTRYPOINT ["/usr/local/bin/iroh-relay", "--config-path", "/etc/iroh-relay/iroh-relay.toml"]
