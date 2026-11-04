# Deploying the signaling server

The signaling server (`crates/signaling-server`, binary `syncwatch-signaling`) only relays
small JSON messages (room create/join, SDP offers/answers, ICE candidates) between peers. It
never sees media, chat or files: those travel over WebRTC data channels, peer to peer or through
the user's own TURN relay. It keeps no data on disk.

## Endpoints

- `GET /ws`: WebSocket, JSON messages (`crates/protocol/src/signaling.rs`).
- `GET /healthz`: `200 ok` for health checks.

## Configuration (environment)

| Variable | Default | Meaning |
|---|---|---|
| `SYNCWATCH_SIGNALING_BIND` | `0.0.0.0:8787` | listen address |
| `SYNCWATCH_TRUST_PROXY` | unset | set to `1` ONLY behind a reverse proxy you control, so the per-IP limits use `X-Forwarded-For` |
| `RUST_LOG` | `info` | log level (logs contain no room tokens or SDP) |

Built-in limits (see `Config` in `crates/signaling-server/src/lib.rs`):
- 32 connections per IP.
- 50 messages/s per connection (burst 200).
- 32 KiB per frame.
- 12 members per room.
- 60 s idle timeout (clients ping every 20 s).
- 30 s host grace period before a room closes.

## Build and run with Docker

The image is pinned by digest, runs as a non-root user (uid 10001) and has a read-only root
filesystem in the compose file.

```
docker build -f deploy/signaling/Dockerfile -t syncwatch-signaling .
docker run --rm -p 127.0.0.1:8787:8787 syncwatch-signaling
curl http://127.0.0.1:8787/healthz
```

## TLS with Caddy (production)

The app only accepts `wss://` for non-local hosts. Put the container behind a TLS reverse proxy.
Caddy obtains and renews certificates automatically. Example `Caddyfile` (replace the host name):

```
signal.example.org {
	encode zstd gzip
	reverse_proxy /ws 127.0.0.1:8787
	reverse_proxy /healthz 127.0.0.1:8787
	respond 404
}
```

Then run the container with `SYNCWATCH_TRUST_PROXY=1` and the port bound to `127.0.0.1` only.
Caddy proxies WebSocket upgrades without extra settings.

Recommended extras:
- Firewall: open only 80/443 (Caddy) to the internet; keep 8787 local.
- Rate limits at the edge, if you have a CDN or a WAF, on top of the built-in per-IP limits.
- Point the app at the server: set the compiled-in default URL
  (`DEFAULT_SIGNALING_URL` in `crates/net/src/signaling.rs`) at release time, or use
  `SYNCWATCH_SIGNALING_URL` for development builds.

## Local development stack

`deploy/compose.dev.yml` runs the signaling server and a local coturn for relay-only tests that
cost no Metered quota. The coturn user in that file is a local placeholder for the container
only.

```
docker compose -f deploy/compose.dev.yml up -d --build
docker compose -f deploy/compose.dev.yml down
```

`scripts/dev/net-lab.ps1` starts and stops what it needs by itself (see docs/build-guide.md).
