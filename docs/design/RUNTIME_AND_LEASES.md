# Runtime and leases

## Ownership

Claviron hosts `MeatusService` as a native runnable. Quincy owns QUIC and TUN
IO. Meatus owns session identity and optional DNAT selection. Scutum owns
authorization, shared/dedicated resource admission, kernel rules and expiry. No HTTP
listener or application runner is required. Process supervision is not enabled
by the current executable.

Each authenticated, IP-assigned connection receives a distinct ID containing
the service incarnation and a monotonic sequence. Two connections from the same
principal remain two sessions. Quincy calls a synchronous, transport-neutral
observer; the observer only updates bounded local state and signals a worker.
Neither connect nor disconnect waits for Scutum.

If observation capacity is exceeded, DNAT selection becomes unavailable until
all connections have released. The VPN keeps working. This prevents an incomplete
session list from silently becoming an authoritative grant list.

## DNAT capability

Omit `dnat` to run the VPN without firewall integration. When enabled,
`principals` maps authenticated usernames to a resource, `dnat_ip`, `mode` and
integer `priority`. Higher priority wins among connected users of the same
resource. Equal-priority shared users form a pool; equal-priority policies for
one resource must agree on mode and DNAT IP. In dedicated mode,
`duplicate_session` selects `keep_existing` or `replace_existing` among ties.
Connections not selected for DNAT still have a working VPN.

When a higher-priority dedicated user arrives, Meatus releases its lower-priority
shared leases before acquisition. When that user leaves, the remaining connected
users become eligible again. Scutum receives VPN IP, DNAT IP, mode, resource and
opaque session ID, never username or priority. The resource grant fixes ports,
protocol and network bounds on the firewall side.

If Scutum rejects acquisition with an occupying VPN IP, Meatus preserves that
structured result. It releases the returned owned lease only if its current
selection no longer wants that IP/mode on the resource, then requests again after
confirmed removal. Foreign lease IDs are not disclosed or released. Scutum never
silently evicts a target to satisfy an acquisition.

The independent worker coalesces current session state. It releases obsolete
binding IDs, requests missing bindings, and renews all current IDs in one bounded
batch. Lifecycle hooks never wait for command replies. While one invoke is pending,
the local state can continue changing; the next pass uses the latest state.

`renew_interval_ms` and `batch_limit` are required. There is no implicit duration.
The renewal interval must be shorter than the TTL returned by Scutum. This same
declared cadence bounds reattachment attempts; no hidden reconnect timer exists.
`max_sessions`, `max_name_bytes`, and Link/invoke geometry are also explicit.

An expired ID is not renewed or automatically reacquired by the renewal loop.
A new session is a new acquisition. A lost response is not proof that no rule
was installed: repeating acquisition for the same active session/resource returns
the existing binding without extending its TTL.

Normal release explicitly removes the binding by ID. No async operation runs in
`Drop`. Orderly service shutdown stops VPN admission, finishes connection cleanup,
then submits releases and drains accepted invokes. No termination deadline is
invented. If the process dies before release, Scutum's lease expires independently.

## Access

Meatus can run under its own OS account. Owner-only Link access is the default
mode; group mode requires explicit owner UID, group ID and an operator-provisioned
IPC directory matching Scutum. Only this dedicated Link is shared. Database
files and unrelated fleet resources remain private. Link access grants no
firewall privilege.
All Link group writers are trusted; a lane name is not an authentication boundary.

Quincy still needs operating-system permission to create/configure its TUN
interface. On Linux, `CAP_NET_ADMIN` also permits firewall changes in that network
namespace: a separate UID alone does not remove this authority. A deployment
requiring a strict kernel boundary must isolate the VPN network namespace or
provide an appropriately preconfigured interface. Meatus does not install
capabilities, modify system users or configure host firewall privileges.

The API describes a leased resource and a session, not Quincy internals. Scutum
has no Quincy or Meatus dependency. Remote network management is not implemented
by a local Link; an independently authenticated gateway can be added later.
