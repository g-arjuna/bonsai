#!/usr/bin/env python3
"""WP6.5A storage/performance measurements (WP65-15/M2.5).

Drives a LIVE disposable Bonsai core through synthetic syslog workloads
and measures the required metrics at 10k and 100k scale:

  - ingestion throughput (msgs/s, wall time)
  - persistence latency proxy (per-batch flush observation)
  - embedding write-back throughput + pending-enrichment drain
  - peak RSS of the core process
  - graph physical bytes, vector payload bytes, registry bytes,
    syslog archive bytes
  - provider requests/texts (dedup stats)
  - similarity retrieval latency
  - enrichment DISABLED (ingest only) vs ENABLED (ingest+worker) comparison

Usage:
    python3 scripts/wp65a_measurements.py --workdir /tmp/wp65a-meas \
        --bonsai-bin target/debug/bonsai --scale 10000 \
        --enrichment on|off --core-pid <pid> --api-url http://...

The caller starts the core (see --print-config for the TOML to use) and
passes its pid + api url. Emits JSON to --report.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import socket
import sys
import time
from pathlib import Path

import numpy as np

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "python"))
sys.path.insert(0, str(Path(__file__).resolve().parent))

import bonsai_ml.syslog_embedding_worker as worker  # noqa: E402
from bonsai_ml.message_templates import TemplateDedup, TemplateRegistry  # noqa: E402
from e2e_wp65a_embedding import DeterministicEmbedder  # noqa: E402


def rss_mb(pid: int) -> float:
    try:
        with open(f"/proc/{pid}/status") as f:
            for line in f:
                if line.startswith("VmRSS:"):
                    return int(line.split()[1]) / 1024.0
    except OSError:
        pass
    return -1.0


def dir_bytes(path: str) -> int:
    total = 0
    p = Path(path)
    if p.is_file():
        return p.stat().st_size
    if p.is_dir():
        for f in p.rglob("*"):
            if f.is_file():
                total += f.stat().st_size
    return total


def corpus(n: int, unique_frac: float, seed: int = 11):
    """Repetitive + high-cardinality mix, same shape as dedup_benchmark."""
    import random

    rng = random.Random(seed)
    templates = [
        "BGP neighbor {ip} hold timer expired",
        "interface {iface} changed state to {state}",
        "BGP error code {code} received from peer {ip}",
        "OSPF adjacency {state} on {iface} router-id {rid}",
        "SNMP trap {oid} from {ip} severity {sev}",
    ]
    states = ["up", "down"]
    n_unique = int(n * unique_frac)
    msgs = []
    for i in range(n):
        if i < n_unique:
            # high-cardinality: unique ip+iface+code each time
            msgs.append(
                f"<134>Nov 16 12:{i//3600%24:02d}:{i//60%60:02d}:{i%60:02d} "
                f"leaf-{i%500:03d} BGP[{1000+i%9000}]: BGP error code "
                f"{i%16} received from peer 10.{i%256}.{(i*7)%256}.{(i*13)%250+1} "
                f"iface Ethernet1/{i%24+1} rid 10.255.{i%256}.{(i*3)%256} "
                f"state {states[i%2]} oid 1.3.6.1.4.1.{i%65536} sev {i%8}"
            )
        else:
            t = templates[i % len(templates)]
            msgs.append(
                f"<134>Nov 16 12:{i//3600%24:02d}:{i//60%60:02d}:{i%60:02d} "
                f"leaf-{i%50:02d} BGP[{i%5000}]: "
                + t.format(
                    ip=f"10.{i%3}.{(i*11)%256}.{(i*17)%250+1}",
                    iface=f"Ethernet1/{i%24+1}",
                    state=states[i % 2],
                    code=i % 16,
                    rid=f"10.200.{i%256}.{(i*5)%256}",
                    oid=f"1.3.6.1.4.1.{(i*31)%65536}",
                    sev=i % 8,
                )
            )
    return msgs


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--workdir", required=True)
    ap.add_argument("--api-url", required=True)
    ap.add_argument("--syslog-udp", required=True)
    ap.add_argument("--scale", type=int, required=True)
    ap.add_argument("--unique-frac", type=float, default=0.05)
    ap.add_argument("--enrichment", choices=["on", "off"], required=True)
    ap.add_argument("--core-pid", type=int, required=True)
    ap.add_argument("--graph-path", required=True)
    ap.add_argument("--archive-path", required=True)
    ap.add_argument("--registry", required=True)
    ap.add_argument("--report", required=True)
    ap.add_argument("--batch", type=int, default=2000)
    args = ap.parse_args()

    workdir = Path(args.workdir)
    workdir.mkdir(parents=True, exist_ok=True)
    out: dict = {"scale": args.scale, "unique_frac": args.unique_frac,
                 "enrichment": args.enrichment}

    # ── Phase 1: ingestion ────────────────────────────────────────────
    msgs = corpus(args.scale, args.unique_frac)
    host, port = args.syslog_udp.rsplit(":", 1)
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.setsockopt(socket.SOL_SOCKET, socket.SO_SNDBUF, 4 * 1024 * 1024)
    t0 = time.monotonic()
    peak_rss = rss_mb(args.core_pid)
    sent = 0
    # Pace datagrams: an un-paced 200k msg/s UDP burst overflows the
    # receiver socket buffer and silently drops most of the corpus,
    # which would fabricate an artificially fast "ingestion" number.
    # ~10k msgs/s is still far above steady-state syslog rates.
    pace = 0.0001  # 10k msgs/s
    for m in msgs:
        sock.sendto(m.encode(), (host, int(port)))
        sent += 1
        if sent % 500 == 0:
            time.sleep(pace * 500)
            peak_rss = max(peak_rss, rss_mb(args.core_pid))
    sock.close()
    ingest_wall = time.monotonic() - t0
    # Settle: wait until the pending-unembedded count stops growing
    # (write coordinator drains its 256-row/1s batches into the graph).
    import requests

    def pending_count():
        try:
            r = requests.get(
                f"{args.api_url}/api/events/unembedded?type=syslog&limit=1000",
                timeout=15,
            ).json()
            return len(r.get("events", []))
        except Exception:
            return -1

    settle_deadline = time.monotonic() + max(60, args.scale / 100)
    last = -2
    stable = 0
    while time.monotonic() < settle_deadline:
        cur = pending_count()
        if cur >= 0 and cur == last:
            stable += 1
            if stable >= 3:
                break
        else:
            stable = 0
        last = cur
        time.sleep(2)
    time.sleep(2)
    peak_rss = max(peak_rss, rss_mb(args.core_pid))

    out["ingest"] = {
        "messages": sent,
        "wall_s": round(ingest_wall, 2),
        "msgs_per_s": round(sent / ingest_wall, 1) if ingest_wall else 0,
        "peak_core_rss_mb": round(peak_rss, 1),
    }
    out["bytes_after_ingest"] = {
        "graph": dir_bytes(args.graph_path),
        "archive": dir_bytes(args.archive_path),
    }

    if args.enrichment == "off":
        out["enrichment"] = "disabled — ingest-only measurement"
    else:
        # ── Phase 2: worker drain (embedding write-back) ──────────────
        embedder = DeterministicEmbedder()
        registry = TemplateRegistry(path=args.registry)
        dedup = TemplateDedup(registry=registry, embedder=embedder, scope="meas")

        # pending age proxy: oldest unembedded event timestamp
        def pending_stats():
            import requests

            r = requests.get(
                f"{args.api_url}/api/events/unembedded?type=syslog&limit=1000",
                timeout=20,
            ).json()
            evs = r.get("events", [])
            return len(evs)

        cycles = 0
        total_embedded = 0
        cycle_lat = []
        provider_texts0 = dedup.metrics.provider_texts_embedded
        t_drain = time.monotonic()
        pending0 = pending_stats()
        while True:
            c0 = time.monotonic()
            events = worker._fetch_unembedded_events(args.api_url, limit=200)
            if not events:
                break
            rep = {}
            n = worker._embed_and_post(
                args.api_url, events, embedder, dedup, report=rep
            )
            total_embedded += n
            cycles += 1
            cycle_lat.append(time.monotonic() - c0)
            if cycles > 2000:
                break
        drain_wall = time.monotonic() - t_drain
        peak_rss = max(peak_rss, rss_mb(args.core_pid))
        lat_sorted = sorted(cycle_lat)

        def pct(p):
            if not lat_sorted:
                return 0.0
            return round(lat_sorted[min(len(lat_sorted) - 1, int(len(lat_sorted) * p))] * 1000, 1)

        out["enrichment"] = {
            "cycles": cycles,
            "embedded_records": total_embedded,
            "pending_at_start": pending0,
            "drain_wall_s": round(drain_wall, 2),
            "writeback_records_per_s": round(total_embedded / drain_wall, 1)
            if drain_wall
            else 0,
            "cycle_latency_ms_p50": pct(0.5),
            "cycle_latency_ms_p95": pct(0.95),
            "cycle_latency_ms_p99": pct(0.99),
            "provider_calls": dedup.metrics.provider_calls,
            "provider_texts_embedded": dedup.metrics.provider_texts_embedded
            - provider_texts0,
            "template_cardinality": dedup.stats_snapshot().get(
                "template_cardinality"
            ),
            "template_occurrences": dedup.stats_snapshot().get(
                "template_occurrences"
            ),
        }
        out["bytes_after_enrichment"] = {
            "graph": dir_bytes(args.graph_path),
            "archive": dir_bytes(args.archive_path),
            "registry": dir_bytes(args.registry),
        }

        # ── Phase 3: similarity latency ───────────────────────────────
        # find one embedded event via unembedded is impossible (all done);
        # use similarity on an event id harvested from a fresh fetch of
        # a single probe message.
        import requests
        import socket as _s

        s = _s.socket(_s.AF_INET, _s.SOCK_DGRAM)
        s.sendto(
            b"<134>Nov 16 13:00:00 probe-host BGP[1]: BGP neighbor 10.77.0.1 hold timer expired",
            tuple(args.syslog_udp.rsplit(":", 1)[0].split(":"))
            if False
            else (args.syslog_udp.rsplit(":", 1)[0], int(args.syslog_udp.rsplit(":", 1)[1])),
        )
        s.close()
        time.sleep(4)
        evs = worker._fetch_unembedded_events(args.api_url, limit=10)
        if evs:
            worker._embed_and_post(
                args.api_url, evs, embedder, dedup, report={}
            )
            eid = str(evs[0].get("id") or evs[0].get("event_id"))
            lat = []
            for _ in range(20):
                t = time.monotonic()
                requests.get(
                    f"{args.api_url}/api/ml/similar-events?event_id={eid}"
                    "&limit=10&min_similarity=0.0",
                    timeout=20,
                )
                lat.append((time.monotonic() - t) * 1000)
            lat.sort()
            out["similarity_latency_ms"] = {
                "p50": round(lat[len(lat) // 2], 1),
                "p95": round(lat[int(len(lat) * 0.95)], 1),
                "max": round(lat[-1], 1),
            }
        registry.close()

    peak_rss = max(peak_rss, rss_mb(args.core_pid))
    out["peak_core_rss_mb"] = round(peak_rss, 1)
    Path(args.report).write_text(json.dumps(out, indent=2))
    print(json.dumps(out, indent=2)[:3000])
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
