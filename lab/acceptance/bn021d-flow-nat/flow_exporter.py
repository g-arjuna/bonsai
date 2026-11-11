#!/usr/bin/env python3
"""BN-021d flow-exporter lab sender.

Crafts NetFlow v9 / IPFIX UDP datagrams and sends them to the Bonsai
collector through the lab NAT. Runs inside exporter containers on the
private side of bn021d-nat; the collector only ever sees the translated
source (10.241.0.200:<port>).

Scenarios (selected with --scenario):
  nat-a       exporter A: v9 + ipfix (same numeric domain), port change,
              v9 uptime-restart refresh.
  collide     exporter B: same v9 source-id/template-id numbers as A but
              a DIFFERENT template definition; ipfix odid collision.
  unbound     exporter C: plausible identity claims, no verified ingress.
  data-only   resend one data record for an already-known template
              (used after collector restart to prove stale templates
              are gone).
  template    send only template definitions (used to re-arm a context
              after restart/port-change).

Packet layouts mirror tests/bn021d_flow_exporter_isolation.rs.
"""

import argparse
import socket
import struct
import sys
import time

STD_FIELDS = [
    (8, 4),   # SOURCE_IPV4_ADDRESS
    (12, 4),  # DESTINATION_IPV4_ADDRESS
    (7, 2),   # SOURCE_TRANSPORT_PORT
    (11, 2),  # DESTINATION_TRANSPORT_PORT
    (4, 1),   # PROTOCOL_IDENTIFIER
    (1, 4),   # OCTET_DELTA_COUNT
    (2, 4),   # PACKET_DELTA_COUNT
    (22, 4),  # FLOW_START_MILLISECOND (v9 uptime-relative)
    (21, 4),  # FLOW_END_MILLISECOND
]

# Same template-id collision, different definition: no time fields.
ALT_FIELDS = [
    (8, 4),
    (12, 4),
    (7, 2),
    (11, 2),
    (4, 1),
    (1, 4),
    (2, 4),
]


def enc_record(src, dst, sport, dport, proto, nbytes, npkts, first=0, last=0,
               timed=True):
    r = struct.pack("!4s4sHHBII", src, dst, sport, dport, proto,
                    nbytes, npkts)
    if timed:
        r += struct.pack("!II", first, last)
    return r


def v9_template_record(tid, fields):
    r = struct.pack("!HH", tid, len(fields))
    for t, l in fields:
        r += struct.pack("!HH", t, l)
    return r


def v9_packet(source_id, uptime_ms, unix_secs, seq, sets):
    p = struct.pack("!HHIII", 9, len(sets), uptime_ms, unix_secs,
                    seq) + struct.pack("!I", source_id)
    for fsid, data in sets:
        p += struct.pack("!HH", fsid, len(data) + 4) + data
    return p


def ipfix_field(ie, flen, pen=None):
    raw = ie | 0x8000 if pen is not None else ie
    f = struct.pack("!HH", raw, flen)
    if pen is not None:
        f += struct.pack("!I", pen)
    return f


def ipfix_template_record(tid, fields):
    return struct.pack("!HH", tid, len(fields)) + b"".join(fields)


def ipfix_packet(domain_id, export_time, seq, sets):
    body = b"".join(struct.pack("!HH", sid, len(d) + 4) + d
                    for sid, d in sets)
    return struct.pack("!HHIII", 10, 16 + len(body), export_time, seq,
                       domain_id) + body


def std_ipfix_fields():
    return [ipfix_field(*f) for f in STD_FIELDS]


def send_pkt(sock, target, pkt, label=""):
    sock.sendto(pkt, target)
    print(f"sent {label or len(pkt)}B -> {target}", flush=True)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--target", required=True, help="collector ip:port")
    ap.add_argument("--scenario", required=True,
                    choices=["nat-a", "collide", "unbound",
                             "data-only", "template"])
    ap.add_argument("--sport", type=int, default=49152)
    ap.add_argument("--src-ip", default="192.0.2.1")
    ap.add_argument("--dst-ip", default="198.51.100.1")
    args = ap.parse_args()

    target = tuple(args.target.rsplit(":", 1))
    target = (target[0], int(target[1]))
    src_ip = socket.inet_aton(args.src_ip)
    dst_ip = socket.inet_aton(args.dst_ip)

    def sock_on(port):
        s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        s.bind(("0.0.0.0", port))
        return s

    seq = [100]

    def next_seq():
        seq[0] += 1
        return seq[0]

    now = int(time.time())

    if args.scenario == "nat-a":
        s = sock_on(args.sport)
        # 1. v9 template + data, source-id 7, template 400.
        tmpl = v9_template_record(400, STD_FIELDS)
        tset = struct.pack("!HH", 0, len(tmpl) + 4) + tmpl
        rec = enc_record(src_ip, dst_ip, 40000, 443, 6, 1500, 10,
                         first=1000, last=2000)
        dset = struct.pack("!HH", 400, len(rec) + 4) + rec
        send_pkt(s, target, v9_packet(7, 500_000, now, next_seq(),
                                      [(0, tset[4:]), (400, dset[4:])]),
                 "v9 tmpl+data sid=7 tid=400")
        # 2. IPFIX same numeric domain (odid=7), template 600.
        itmpl = ipfix_template_record(600, std_ipfix_fields())
        iset = struct.pack("!HH", 2, len(itmpl) + 4) + itmpl
        irec = enc_record(socket.inet_aton("192.0.2.11"), dst_ip,
                          40001, 443, 6, 900, 5)
        idata = struct.pack("!HH", 600, len(irec) + 4) + irec
        send_pkt(s, target, ipfix_packet(7, now, next_seq(),
                                         [(2, iset[4:]), (600, idata[4:])]),
                 "ipfix tmpl+data odid=7 tid=600")
        # 3. Source-port change: new socket, same template numbers —
        #    old templates must NOT be inherited.
        s2 = sock_on(args.sport + 1)
        # data-only first: must be dropped (unknown template on new ctx)
        rec2 = enc_record(src_ip, dst_ip, 40002, 443, 6, 700, 3,
                          first=3000, last=4000)
        d2 = struct.pack("!HH", 400, len(rec2) + 4) + rec2
        send_pkt(s2, target, v9_packet(7, 500_100, now, next_seq(),
                                       [(400, d2[4:])]),
                 "v9 data-only on NEW port (expect drop)")
        time.sleep(0.5)
        # then template + data on the new port: must work
        send_pkt(s2, target, v9_packet(7, 500_100, now, next_seq(),
                                       [(0, tset[4:]), (400, d2[4:])]),
                 "v9 tmpl+data on NEW port")
        # 4. v9 restart: uptime decreases -> templates cleared; data
        #    before refresh must drop, refreshed template must work.
        rec3 = enc_record(src_ip, dst_ip, 40003, 443, 6, 800, 4,
                          first=5000, last=6000)
        d3 = struct.pack("!HH", 400, len(rec3) + 4) + rec3
        send_pkt(s, target, v9_packet(7, 100, now, next_seq(),
                                      [(400, d3[4:])]),
                 "v9 data after uptime restart (expect drop)")
        time.sleep(0.5)
        send_pkt(s, target, v9_packet(7, 200, now, next_seq(),
                                      [(0, tset[4:]), (400, d3[4:])]),
                 "v9 tmpl+data after restart")

    elif args.scenario == "collide":
        s = sock_on(args.sport)
        # Same v9 source-id 7 and template-id 400 as exporter A, but a
        # DIFFERENT field layout (no timestamps -> 21-byte records).
        atmpl = v9_template_record(400, ALT_FIELDS)
        tset = struct.pack("!HH", 0, len(atmpl) + 4) + atmpl
        arec = enc_record(socket.inet_aton("203.0.113.1"), dst_ip,
                          50000, 8443, 17, 640, 8, timed=False)
        dset = struct.pack("!HH", 400, len(arec) + 4) + arec
        send_pkt(s, target, v9_packet(7, 600_000, now, next_seq(),
                                      [(0, tset[4:]), (400, dset[4:])]),
                 "v9 collide tmpl+data sid=7 tid=400 alt-fields")
        # ipfix odid=7 tid=950 with standard fields.
        itmpl = ipfix_template_record(950, std_ipfix_fields())
        iset = struct.pack("!HH", 2, len(itmpl) + 4) + itmpl
        irec = enc_record(socket.inet_aton("203.0.113.2"), dst_ip,
                          50001, 443, 6, 1200, 6)
        idata = struct.pack("!HH", 950, len(irec) + 4) + irec
        send_pkt(s, target, ipfix_packet(7, now, next_seq(),
                                         [(2, iset[4:]), (950, idata[4:])]),
                 "ipfix collide tmpl+data odid=7 tid=950")

    elif args.scenario == "unbound":
        s = sock_on(args.sport)
        tmpl = v9_template_record(777, STD_FIELDS)
        tset = struct.pack("!HH", 0, len(tmpl) + 4) + tmpl
        rec = enc_record(src_ip, dst_ip, 60000, 22, 6, 2200, 11,
                         first=1000, last=2000)
        dset = struct.pack("!HH", 777, len(rec) + 4) + rec
        send_pkt(s, target, v9_packet(99, 700_000, now, next_seq(),
                                      [(0, tset[4:]), (777, dset[4:])]),
                 "v9 unbound sid=99 tid=777")

    elif args.scenario == "data-only":
        s = sock_on(args.sport)
        rec = enc_record(src_ip, dst_ip, 40004, 443, 6, 333, 2,
                         first=7000, last=8000)
        dset = struct.pack("!HH", 400, len(rec) + 4) + rec
        send_pkt(s, target, v9_packet(7, 900_000, now, next_seq(),
                                      [(400, dset[4:])]),
                 "v9 data-only sid=7 tid=400")

    elif args.scenario == "template":
        s = sock_on(args.sport)
        tmpl = v9_template_record(400, STD_FIELDS)
        tset = struct.pack("!HH", 0, len(tmpl) + 4) + tmpl
        send_pkt(s, target, v9_packet(7, 900_100, now, next_seq(),
                                      [(0, tset[4:])]),
                 "v9 template-only sid=7 tid=400")

    time.sleep(0.3)


if __name__ == "__main__":
    main()
