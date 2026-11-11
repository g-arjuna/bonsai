#!/usr/bin/env python3
"""BN-021f lab: open a SECOND authenticated BMP session presenting the
same client certificate as an exporter, send a BMPv3 Initiation, then
idle. Used to prove that two sessions sharing one verified principal
coexist in the same vexp:* scope and are BOTH revoked together.

Runs inside an exporter container (has /tls mounts). Stdlib only.

Usage:
    python3 second_session.py --cert /tls/client-a.crt --key /tls/client-a.key \
        --ca /tls/ca.crt --sysname frr-a --target 10.240.0.20:11019
"""
import argparse
import socket
import ssl
import struct
import sys
import time


def tlv(t, v):
    return struct.pack(">HH", t, len(v)) + v


def initiation(sysname):
    body = tlv(1, b"FRR 10.6.1_lab") + tlv(2, sysname.encode())
    msg = struct.pack(">B", 3) + struct.pack(">I", 6 + len(body)) + bytes([4]) + body
    return msg


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--cert", required=True)
    p.add_argument("--key", required=True)
    p.add_argument("--ca", required=True)
    p.add_argument("--sysname", required=True)
    p.add_argument("--target", default="10.240.0.20:11019")
    args = p.parse_args()
    host, port = args.target.rsplit(":", 1)

    ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
    ctx.minimum_version = ssl.TLSVersion.TLSv1_2
    ctx.load_verify_locations(cafile=args.ca)
    ctx.load_cert_chain(certfile=args.cert, keyfile=args.key)
    ctx.check_hostname = True
    ctx.verify_mode = ssl.CERT_REQUIRED

    raw = socket.create_connection((host, int(port)), timeout=15)
    tls = ctx.wrap_socket(raw, server_hostname="bonsai")
    tls.sendall(initiation(args.sysname))
    print(f"second session up: sysname={args.sysname}", flush=True)
    # Idle: keep the TCP/TLS session open until Bonsai closes it.
    while True:
        try:
            data = tls.recv(4096)
        except OSError:
            break
        if not data:
            break
        time.sleep(0.2)
    print("second session closed by peer", flush=True)


if __name__ == "__main__":
    main()
