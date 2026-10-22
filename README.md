# Meatus

A native Claviron VPN service powered by Quincy. Meatus runs its own Rust
service on the Claviron runtime; it does not use an HTTP application runner.

DNAT is an optional capability. Meatus chooses which VPN session receives a
configured resource, requests a lease from Scutum, renews leases in a batch,
and releases each binding by ID when its session ends. Firewall authority stays
with Scutum. A slow or unavailable Scutum does not delay VPN connections.

Shared leases form a round-robin pool at one public IP:port; dedicated leases
reserve it for one VPN IP. Meatus applies user priorities locally and releases
obsolete leases before requesting replacements. Scutum sees IPs and leases,
not usernames. VPN connections never wait for firewall replies.

See [runtime and lease contract](docs/design/RUNTIME_AND_LEASES.md) for lifecycle,
configuration and deployment boundaries.


## License

AGPL-3.0-or-later. Meatus embeds [Quincy](https://github.com/quincy-rs/quincy).
Scutum is a separate service; Meatus depends only on its transport-neutral
management contract, not its firewall implementation.
