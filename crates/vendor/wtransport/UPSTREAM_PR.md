# Upstream PR draft for BiagioFesta/wtransport

Not opened yet. Ready to send as-is. The change is `upstream.patch` in this directory, relative to
the upstream repository root (apply with `git apply upstream.patch` on commit `d022a6ad`), without
our `[workspace]` table and vendoring files.

---

**Title:** Add `Endpoint<Client>::rebind` for connection migration

**Body:**

Client endpoints can't replace their UDP socket today: the `quinn::Endpoint` is private and only
server endpoints have `reload_config(.., rebind)`.

A client on a device that switches networks (Wi-Fi to cellular, a VPN coming up) needs this to keep
its connections: binding a fresh socket and calling `quinn::Endpoint::rebind` makes quinn move every
connection to the new local address (new connection ID, probe, path validation by the server).
Relying on a wildcard socket to follow the new route isn't enough when the old socket can no longer
reach the server (an OS that invalidates it with its interface, or an address family that is gone
on the new network).

This adds `Endpoint<endpoint_side::Client>::rebind(std::net::UdpSocket)`, forwarding to
`quinn::Endpoint::rebind`. On error the old socket is kept, as in quinn.
