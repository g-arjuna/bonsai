#!/usr/bin/env python3
"""BN-021f lab: authenticated BMP ingress shim.

FRR 10.6's BMP client (bgpd_bmp) speaks plain TCP only — it has no TLS
support. This shim runs inside each exporter container:

    FRR BMP (plaintext TCP) -> 127.0.0.1:<listen>
        -> mutual-TLS client -> <connect> (through the lab NAT)
        -> Bonsai authenticated BMP listener

The shim presents a per-exporter client certificate and VERIFIES the
Bonsai server certificate against the lab CA (check_hostname against
the configured server name). It forwards bytes bidirectionally and
preserves TCP close semantics (half-close propagates so BMP EOF /
Termination behaviour is unchanged).

No third-party modules — stdlib ssl + socket + threading only.

Usage:
    python3 bmp_tls_proxy.py \
        --listen 127.0.0.1:11019 \
        --connect 10.240.0.20:11019 \
        --cert /tls/client-a.crt --key /tls/client-a.key \
        --ca /tls/ca.crt --server-name bonsai
"""

import argparse
import socket
import ssl
import sys
import threading


def pump(src, dst):
    """Copy bytes src->dst until EOF, then half-close dst."""
    try:
        while True:
            data = src.recv(65536)
            if not data:
                break
            dst.sendall(data)
    except OSError:
        pass
    finally:
        try:
            dst.shutdown(socket.SHUT_WR)
        except OSError:
            pass
        try:
            src.close()
        except OSError:
            pass


def handle(client_sock, args, ctx):
    try:
        upstream = socket.create_connection(
            (args.connect_host, args.connect_port), timeout=15
        )
    except OSError as e:
        print(f"upstream connect failed: {e}", file=sys.stderr, flush=True)
        client_sock.close()
        return
    try:
        tls = ctx.wrap_socket(
            upstream,
            server_hostname=args.server_name,
            do_handshake_on_connect=True,
        )
    except ssl.SSLError as e:
        print(f"TLS handshake to bonsai failed: {e}", file=sys.stderr, flush=True)
        upstream.close()
        client_sock.close()
        return
    # Server cert is verified by the SSL context (CERT_REQUIRED +
    # check_hostname). Peer cert presented = this exporter's credential.
    t1 = threading.Thread(target=pump, args=(client_sock, tls), daemon=True)
    t2 = threading.Thread(target=pump, args=(tls, client_sock), daemon=True)
    t1.start()
    t2.start()
    t1.join()
    t2.join()
    try:
        tls.close()
    except OSError:
        pass
    try:
        client_sock.close()
    except OSError:
        pass


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--listen", required=True, help="host:port to accept FRR BMP on")
    p.add_argument("--connect", required=True, help="host:port of bonsai TLS BMP")
    p.add_argument("--cert", required=True)
    p.add_argument("--key", required=True)
    p.add_argument("--ca", required=True)
    p.add_argument("--server-name", required=True,
                   help="hostname/IP used to verify the bonsai server cert")
    args = p.parse_args()

    args.listen_host, args.listen_port = args.listen.rsplit(":", 1)
    args.listen_port = int(args.listen_port)
    args.connect_host, args.connect_port = args.connect.rsplit(":", 1)
    args.connect_port = int(args.connect_port)

    ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
    ctx.minimum_version = ssl.TLSVersion.TLSv1_2
    ctx.load_verify_locations(cafile=args.ca)
    ctx.load_cert_chain(certfile=args.cert, keyfile=args.key)
    # Verify the server certificate AND its hostname — the shim must not
    # accept an arbitrary TLS endpoint as "bonsai".
    ctx.check_hostname = True
    ctx.verify_mode = ssl.CERT_REQUIRED

    lsock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    lsock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    lsock.bind((args.listen_host, args.listen_port))
    lsock.listen(8)
    print(f"bmp_tls_proxy: {args.listen} -> tls {args.connect} "
          f"(cert={args.cert})", flush=True)
    while True:
        client, _ = lsock.accept()
        threading.Thread(
            target=handle, args=(client, args, ctx), daemon=True
        ).start()


if __name__ == "__main__":
    main()
