# syncwatch-signaling: minimal non-root image.
#   docker build -f deploy/signaling/Dockerfile -t syncwatch-signaling .
# Run behind a TLS reverse proxy (see deploy/README.md); the container speaks plain HTTP/WS.

FROM rust:1.99.1-slim-trixie@sha256:ea0999ff67355d958b8799b6751c879a6d770076d26047dca66f6e51e77930d3 AS build
WORKDIR /src
# The workspace manifest needs every member's Cargo.toml; only the signaling crate is built.
COPY . .
RUN cargo build --locked --release -p syncwatch-signaling \
 && strip target/release/syncwatch-signaling

FROM debian:trixie-slim@sha256:f6b08b09a05725f9431502d3b5714f5c4576e82c1a84fa1debe0c24a7e0c3ec4
RUN useradd --system --uid 10001 --no-create-home --shell /usr/sbin/nologin signaling
COPY --from=build /src/target/release/syncwatch-signaling /usr/local/bin/syncwatch-signaling
USER 10001:10001
ENV SYNCWATCH_SIGNALING_BIND=0.0.0.0:8787 RUST_LOG=info
EXPOSE 8787
ENTRYPOINT ["/usr/local/bin/syncwatch-signaling"]
