# CrateTune's iroh relay (device hand-off)

Device hand-off (Settings → Devices, the **Devices** button on the player bar) connects
paired CrateTune installations peer to peer with [iroh](https://github.com/n0-computer/iroh)
QUIC. On the same network devices talk directly (found by mDNS). Across networks they try to
connect directly too, and fall back to a **relay** that forwards their end-to-end encrypted
packets. iroh's public relays and its DNS address lookup are never used: the app uses this
relay, built from the pinned `iroh-relay` crate, or none.

The relay sees each device's IP address and device id (its public key) and how much it
sends; it can't read anything (QUIC/TLS between the devices). It stores nothing.

| File | What |
|---|---|
| `Dockerfile` | builds `iroh-relay` 1.4.1 from crates.io (`--locked`) and runs it as `nonroot` on `gcr.io/distroless/cc-debian12` |
| `docker-compose.yml` | read-only root, `cap_drop: ALL`, `no-new-privileges`, 128 MB / 64 pids / half a CPU, log rotation; plain HTTP on LAN port **3340** |
| `iroh-relay.toml` | relay on, plain HTTP (TLS is Caddy's), QUIC address discovery off, no metrics, open access with per-client rate limits |

## Routing: same host as music-relay, by path

The built-in relay URL is `IROH_RELAY_URL=https://relay.smhcoders.click` (`packaging/app.env`),
the host that already serves `music-relay` (Music Together). iroh always appends its own
paths to the relay URL (it can't live under a prefix), and none of them collide with
music-relay's:

| Path | Served by |
|---|---|
| `/relay` (WebSocket upgrade; also the legacy `/derp`) | iroh-relay |
| `/ping` (latency probe, HTTPS) | iroh-relay |
| `/generate_204` (captive-portal check; the app doesn't ask for it, other iroh clients might) | iroh-relay |
| `/` (WebSocket), `/v1/…`, `/health`, `/healthz`, `/player-fixes.json(.minisig)` | music-relay |

A separate host name (e.g. `iroh.smhcoders.click`) works just as well: route everything to
port 3340 and change `IROH_RELAY_URL` (and announce it; old builds keep the old URL).

## What the coordinator does on the server (dev.home)

1. Copy this folder to the server, e.g. `/srv/cratetune-iroh-relay/`.
2. `docker compose up -d --build` (the first build compiles iroh-relay, a few minutes).
3. Check it locally on the server:
   ```sh
   curl -i http://127.0.0.1:3340/ping           # 200
   curl -i http://127.0.0.1:3340/generate_204   # 204
   docker compose logs --tail 20                # "relay: serving on 0.0.0.0:3340"
   ```

## What the maintainer adds in Caddy (192.168.0.52)

In the existing `relay.smhcoders.click` site block, **before** the catch-all that proxies to
music-relay (Caddy's `handle` blocks are mutually exclusive and sorted by path specificity,
so a named matcher with `handle` is the safe way):

```caddyfile
relay.smhcoders.click {
	@iroh path /relay /relay/* /derp /ping /generate_204
	handle @iroh {
		reverse_proxy <docker-host-LAN-IP>:3340
	}

	handle {
		# … the existing music-relay proxy, unchanged …
	}
}
```

`reverse_proxy` passes the WebSocket upgrade on `/relay` through by itself; no timeouts need
raising (iroh pings the relay every few seconds). Then `caddy reload` (or the container's
equivalent).

Or, with a separate host:

```caddyfile
iroh.smhcoders.click {
	reverse_proxy <docker-host-LAN-IP>:3340
}
```

## Testing it

From any machine:

```sh
curl -i https://relay.smhcoders.click/ping          # 200 from iroh-relay
curl -i https://relay.smhcoders.click/generate_204  # 204
curl -i https://relay.smhcoders.click/health        # still music-relay
```

End to end (two in-process devices that only know each other through the relay; no
direct addresses are exchanged) from a checkout of the app:

```sh
MUSIC_TEST_IROH_RELAY=https://relay.smhcoders.click \
  cargo test -p music-core --release -- --ignored take_over_via_a_relay
```

In the app: Settings → Devices → "Other networks" shows the relay in use; with two
computers on different networks (a phone hotspot is enough), pair them with the link
(the code alone only finds devices on the same network) and use **Continue here**.

## UDP / QUIC address discovery (optional)

iroh relays can also answer *QUIC address discovery* (QAD) on UDP 7842: a device learns its
public IP:port from the relay, which helps two devices behind NATs connect **directly**
(hole punching) instead of staying on the relay. QAD needs TLS on the relay itself (a
certificate for the relay's name) and the UDP port forwarded from the internet; Caddy can't
proxy it. It is off here, and the app doesn't ask relays for it.

Without it everything still works: on the same network devices connect directly anyway, and
across networks they reach each other through the relay (a few hundred bytes per request;
hand-off is a handful of messages). What's lost is the chance of a direct path between
networks, i.e. lower latency and less traffic through the relay, and some hole punching
(UPnP / NAT-PMP port mappings, which the app still tries, recover part of it). To turn it
on later: give iroh-relay a certificate (`[tls]` with `cert_mode = "Manual"`, or
`"LetsEncrypt"` on a host that owns port 443), set `enable_quic_addr_discovery = true`,
publish UDP 7842, and ship a build that sets a QUIC port for the relay
(`RelayConfig::new(url, Some(RelayQuicConfig::default()))` in `handoff/node.rs`).

## Upgrading

Change `IROH_RELAY_VERSION` (Dockerfile, docker-compose.yml) together with the app's
`iroh` version in `crates/music-core/Cargo.toml`; relays and clients of the same 1.x line
interoperate. `docker compose up -d --build`.
