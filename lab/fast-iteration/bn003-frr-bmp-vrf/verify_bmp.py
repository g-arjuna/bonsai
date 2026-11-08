#!/usr/bin/env python3
"""BN-003 live BMP listener: requires real BMPv3 peer-up + NLRI, RD instance 65000:100.

Run INSTEAD of (never concurrently with) the Bonsai listener at 0.0.0.0:11029.
Captures FRR-originated raw bytes to genuine-bmp-stream.bin. Python standard library only.
"""
import argparse
import ipaddress
import socket
import struct
import time
from pathlib import Path

RD_BYTES = struct.pack('!HHI', 0, 65000, 100)
EXPECTED_PREFIX = ipaddress.IPv4Network('10.99.1.0/24')


def read_exact(sock, size, end_time):
    out = bytearray()
    while len(out) != size:
        if time.monotonic() > end_time:
            raise TimeoutError('Timeout reading BMP frame')
        try:
            data = sock.recv(size - len(out))
        except socket.timeout:
            continue
        if not data:
            raise EOFError('BMP peer closed TCP connection')
        out.extend(data)
    return bytes(out)


def update_nlri(payload):
    # BMP route-monitoring payload starts with a standard BGP PDU.
    if len(payload) < 23 or payload[:16] != b'\xff' * 16:
        return []
    bgp_size = struct.unpack_from('!H', payload, 16)[0]
    if bgp_size < 23 or bgp_size > len(payload) or payload[18] != 2:  # UPDATE
        return []
    pkt = payload[:bgp_size]
    withdrawn_size = struct.unpack_from('!H', pkt, 19)[0]
    pos = 21 + withdrawn_size
    if pos + 2 > len(pkt):
        return []
    attr_size = struct.unpack_from('!H', pkt, pos)[0]
    pos += 2 + attr_size
    if pos > len(pkt):
        return []
    prefixes = []
    while pos < len(pkt):
        bits = pkt[pos]
        pos += 1
        if bits > 32:
            return []
        octets = (bits + 7) // 8
        if pos + octets > len(pkt):
            return []
        addr = pkt[pos:pos + octets].ljust(4, b'\x00')
        pos += octets
        prefixes.append(ipaddress.IPv4Network((ipaddress.IPv4Address(addr), bits), strict=False))
    return prefixes


def main():
    ap = argparse.ArgumentParser(description='Real FRR RD-instance BMP v3 acceptance probe')
    ap.add_argument('--bind', default='0.0.0.0')
    ap.add_argument('--port', type=int, default=11029)
    ap.add_argument('--seconds', type=int, default=120)
    ap.add_argument('--capture', type=Path, default=Path('genuine-bmp-stream.bin'))
    args = ap.parse_args()

    deadline = time.monotonic() + args.seconds
    seen_up = False
    seen_route = False
    events = 0
    args.capture.parent.mkdir(parents=True, exist_ok=True)
    with socket.socket() as server, args.capture.open('wb') as capture:
        server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        server.bind((args.bind, args.port))
        server.listen(2)
        server.settimeout(1.0)
        print(f'Listening on {args.bind}:{args.port}; start containerlab in another terminal.', flush=True)
        print(f'Looking for BMP peer type=1, RD=65000:100, BGP NLRI={EXPECTED_PREFIX}', flush=True)
        while time.monotonic() < deadline and not (seen_up and seen_route):
            try:
                client, addr = server.accept()
            except socket.timeout:
                continue
            print('BMP TCP connected from', addr, flush=True)
            with client:
                client.settimeout(1.0)
                while time.monotonic() < deadline and not (seen_up and seen_route):
                    try:
                        hdr = read_exact(client, 6, deadline)
                        version, frame_size, msg_type = struct.unpack('!BIB', hdr)
                        if version != 3 or frame_size < 6 or frame_size > 262144:
                            raise ValueError(f'Invalid BMP header: version={version}, length={frame_size}')
                        payload = read_exact(client, frame_size - 6, deadline)
                    except EOFError:
                        print('BMP TCP disconnected; awaiting reconnect', flush=True)
                        break
                    except TimeoutError:
                        break
                    capture.write(hdr + payload)
                    capture.flush()
                    events += 1
                    if msg_type not in (0, 3) or len(payload) < 42:
                        continue
                    peer_type = payload[0]  # BMP offset 6
                    rd = payload[2:10]   # BMP offsets 8..15
                    flags = payload[1]
                    if peer_type != 1 or rd != RD_BYTES:
                        print(f'other BMP event: msg={msg_type}, peer_type={peer_type}, '
                              f'RD={rd.hex()}', flush=True)
                        continue
                    if msg_type == 3:
                        seen_up = True
                        print('PASS: genuine RD-instance BMP Peer Up, RD=65000:100', flush=True)
                    if msg_type == 0:
                        prefixes = update_nlri(payload[42:])
                        if EXPECTED_PREFIX in prefixes:
                            seen_route = True
                            print(f'PASS: genuine RD-instance BMP Route Monitoring NLRI '
                                  f'{EXPECTED_PREFIX} (L/post-policy flag={bool(flags & 0x40)})', flush=True)
        print(f'Recorded {events} native BMP frames to {args.capture}', flush=True)
    if seen_up and seen_route:
        print('BN-003 WIRE PROOF PASSED: peer-up + real route-monitoring with type=1 and RD=65000:100')
        return 0
    print(f'BN-003 WIRE PROOF FAILED: peer-up={seen_up}, route-monitoring={seen_route}')
    return 1


if __name__ == '__main__':
    raise SystemExit(main())
