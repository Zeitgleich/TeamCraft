#!/usr/bin/env python3
"""A NAT-PMP server (RFC 6886) for the NAT matrix's routers (#127): answers the external address,
and maps UDP ports with nftables rules (a DNAT inward, an SNAT outward that keeps the port), so a
router of the matrix offers port mapping as a home router does. Test only: no lifetimes, no epochs
worth the name, one client. Exits after `seconds`.

Usage: natpmp-router.py <lan_ip> <wan_ip> <client_ip> <seconds>
"""

import socket
import struct
import subprocess
import sys
import time

NAT_PMP_PORT = 5351


def nft(*args: str) -> None:
    subprocess.run(["nft", *args], check=True)


def main() -> None:
    lan_ip, wan_ip, client_ip, seconds = sys.argv[1], sys.argv[2], sys.argv[3], float(sys.argv[4])
    nft("add", "chain", "ip", "nat", "pmp_in", "{ type nat hook prerouting priority -110; }")
    nft("add", "chain", "ip", "nat", "pmp_out", "{ type nat hook postrouting priority 90; }")
    started = time.monotonic()
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.bind((lan_ip, NAT_PMP_PORT))
    sock.settimeout(1.0)
    mapped: set[int] = set()
    while time.monotonic() - started < seconds:
        try:
            request, peer = sock.recvfrom(64)
        except socket.timeout:
            continue
        epoch = int(time.monotonic() - started)
        if request[:2] == b"\x00\x00":
            answer = struct.pack("!BBHI4s", 0, 128, 0, epoch, socket.inet_aton(wan_ip))
        elif len(request) == 12 and request[:2] == b"\x00\x01":
            internal, _suggested, lifetime = struct.unpack("!HHI", request[4:12])
            if lifetime and internal not in mapped:
                nft("add", "rule", "ip", "nat", "pmp_in", "iifname", "wan", "udp", "dport", str(internal),
                    "dnat", "to", f"{client_ip}:{internal}")
                nft("add", "rule", "ip", "nat", "pmp_out", "ip", "saddr", client_ip, "udp", "sport",
                    str(internal), "oifname", "wan", "snat", "to", f"{wan_ip}:{internal}")
                mapped.add(internal)
            external = internal if lifetime else 0
            answer = struct.pack("!BBHIHHI", 0, 129, 0, epoch, internal, external, lifetime)
        else:
            # Unsupported opcode (RFC 6886 §3.5).
            answer = struct.pack("!BBHI", 0, 128 | (request[1] if len(request) > 1 else 0), 5, epoch)
        sock.sendto(answer, peer)


if __name__ == "__main__":
    main()
