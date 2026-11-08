#!/usr/bin/env python3
"""Send one uniquely tagged IPv4 packet through the dedicated cEOS FIB.

Run inside the BN-006 source container. The advertised prefix's next hop is
this same container, so a forwarded packet returns on its selected interface
with TTL decremented.
"""
import argparse
import json
import os
import select
import socket
import struct
import time


def checksum(data):
    if len(data) % 2:
        data += b"\0"
    total = sum(struct.unpack("!%dH" % (len(data) // 2), data))
    while total >> 16:
        total = (total & 0xffff) + (total >> 16)
    return (~total) & 0xffff


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--expect", choices=["forwarded", "absent"], required=True)
    parser.add_argument("--instance", choices=["default", "blue"], default="default")
    args = parser.parse_args()
    interface, local, router, target = (
        ("eth1", "10.206.0.2", "10.206.0.1", "10.77.1.123")
        if args.instance == "default" else
        ("eth2", "10.206.1.2", "10.206.1.1", "10.77.2.123")
    )
    local_ip = socket.inet_aton(local)
    router_ip = socket.inet_aton(router)
    destination = socket.inet_aton(target)
    local_mac = bytes.fromhex(open(f"/sys/class/net/{interface}/address").read().strip().replace(":", ""))
    with socket.socket(socket.AF_PACKET, socket.SOCK_RAW, socket.htons(3)) as sock:
        sock.bind((interface, 0))
        arp = struct.pack("!HHBBH", 1, 0x0800, 6, 4, 1) + local_mac + local_ip + b"\0" * 6 + router_ip
        sock.send(b"\xff" * 6 + local_mac + b"\x08\x06" + arp)
        deadline = time.monotonic() + 8
        router_mac = None
        while time.monotonic() < deadline:
            if not select.select([sock], [], [], 0.5)[0]:
                continue
            packet = sock.recv(65535)
            if len(packet) >= 42 and packet[12:14] == b"\x08\x06" and packet[20:22] == b"\0\x02" and packet[28:32] == router_ip:
                router_mac = packet[22:28]
                break
        if router_mac is None:
            raise RuntimeError("dedicated EOS ARP reply not observed")
        payload = b"bonsai-bn006-fib-" + os.urandom(16)
        udp = struct.pack("!HHHH", 45121, 45122, 8 + len(payload), 0) + payload
        header = struct.pack("!BBHHHBBH4s4s", 0x45, 0, 20 + len(udp), 43521, 0, 64, 17, 0, local_ip, destination)
        header = header[:10] + struct.pack("!H", checksum(header)) + header[12:]
        frame = router_mac + local_mac + b"\x08\x00" + header + udp
        deadline = time.monotonic() + 5
        forwarded = False
        observed = None
        while time.monotonic() < deadline and not forwarded:
            sock.send(frame)
            until = time.monotonic() + 0.5
            while time.monotonic() < until:
                if not select.select([sock], [], [], max(0, until-time.monotonic()))[0]:
                    break
                packet = sock.recv(65535)
                if (len(packet) >= 42 + len(payload) and packet[:6] == local_mac and packet[6:12] == router_mac
                        and packet[12:14] == b"\x08\x00" and packet[22] == 63 and packet[23] == 17
                        and packet[30:34] == destination and packet[42:42+len(payload)] == payload):
                    forwarded = True
                    observed = packet
                    break
        print(json.dumps({"forwarded": forwarded, "expected": args.expect,
                          "instance": args.instance, "destination": target, "sent_frame_hex": frame.hex(),
                          "received_frame_hex": observed.hex() if observed else None}))
        assert forwarded == (args.expect == "forwarded")


if __name__ == "__main__":
    main()
