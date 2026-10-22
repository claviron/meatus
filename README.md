# Meatus

A native Claviron VPN service powered by Quincy. Meatus runs its own Rust
service on the Claviron runtime; it does not use an HTTP application runner.

DNAT is an optional capability. Meatus chooses which VPN session receives a
configured resource, requests a lease from Scutum, renews leases in a batch,
and releases each binding by ID when its session ends. Firewall authority stays
with Scutum. A slow or unavailable Scutum does not delay VPN connections.

The duplicate-session policy is explicit: keep the existing session or replace
it. Replacement releases the old binding before requesting the new one. This
is not load balancing.

See [runtime and lease contract](docs/design/RUNTIME_AND_LEASES.md) for lifecycle,
configuration and deployment boundaries.

## Development

The Quincy lifecycle hook and the owner/group Link APIs currently require the
prepared source checkouts; registry releases alone are not sufficient yet.
`scripts/check.sh` accepts the Quincy and Orbitive source directories explicitly.
It uses command-local Cargo patches, not persistent workspace/global overrides.

The executable accepts `--config` with a Meatus TOML file. `quincy_config` points
to Quincy's ordinary server configuration; keys and user identity files stay
deployment-owned. No service, tunnel or firewall is started by checks.

## License

AGPL-3.0-or-later. Meatus embeds [Quincy](https://github.com/quincy-rs/quincy).
Scutum is a separate service; Meatus depends only on its transport-neutral
management contract, not its firewall implementation.
