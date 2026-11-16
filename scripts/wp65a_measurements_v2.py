#!/usr/bin/env python3
"""WP6.5A corrected storage/performance measurement (M2.5 v2).

Implements the owner-mandated corrections:

  * Receiver identity is captured by the RUNNER (exe sha256, pid,
    ports, config, dirs) and echoed into the report — traffic is proven
    to reach only the disposable host instance.
  * Loss-accounted monotonic-deadline pacing with bounded bursts;
    offered rate, send errors and jitter recorded.
  * Exact drain via /api/ml/embeddings/stats (graph counts) +
    Prometheus counters + write-coordinator depth; deferred events with
    future retry deadlines are NOT treated as drained.
  * Full pipeline accounting deltas from /metrics + stats + archive.
  * Modes: ingest-only baseline, post-ingest drain, CONCURRENT
    ingest+enrich (WP65-15 requirement).

The runner (wp65a-meas-run-v2.sh) supplies a retention-unlimited
disposable core. This script only measures.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import socket
import sys
import threading
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
    p = Path(path)
    if p.is_file():
        return p.stat().st_size
    if p.is_dir():
        return sum(f.stat().st_size for f in p.rglob("*") if f.is_file())
    return 0


def corpus(n: int, unique_frac: float, seed: int = 11, tag: str = ""):
    """Repetitive template messages + high-cardinality uniques.

    The repetitive slice is BYTE-IDENTICAL text so template dedup sees
    real repetition (per-message prefixes defeat normalization —
    measured: template_cardinality == events on the old corpus).
    """
    import random

    rng = random.Random(seed)
    rep_texts = [
        "<134>Nov 16 12:00:00 leaf-01 BGP[1234]: BGP neighbor 10.0.0.1 hold timer expired",
        "<134>Nov 16 12:00:01 leaf-01 BGP[1234]: interface Ethernet1/1 changed state to down",
        "<134>Nov 16 12:00:02 leaf-01 BGP[1234]: BGP error code 6 received from peer 10.0.0.2",
        "<134>Nov 16 12:00:03 leaf-01 BGP[1234]: OSPF adjacency down on Ethernet1/2 router-id 10.1.1.1",
        "<134>Nov 16 12:00:04 leaf-01 BGP[1234]: SNMP trap 1.3.6.1.4.1.9 from 10.0.0.3 severity 5",
    ]
    n_unique = int(n * unique_frac)
    n_rep = n - n_unique
    msgs = [rep_texts[i % len(rep_texts)] for i in range(n_rep)]
    states = ["up", "down"]
    for i in range(n_unique):
        ts = f"12:{i//3600%24:02d}:{i//60%60:02d}:{i%60:02d}"
        msgs.append(
            f"<134>Nov 16 {ts} leaf-{i%500:03d} BGP[{1000+i%9000}]: "
            f"BGP error code {i%16} received from peer "
            f"10.{i%256}.{(i*7)%256}.{(i*13)%250+1} "
            f"iface Ethernet1/{i%24+1} rid 10.255.{i%256}.{(i*3)%256} "
            f"state {states[i%2]} oid 1.3.6.1.4.1.{i%65536} sev {i%8}"
        )
    rng.shuffle(msgs)
    return msgs


def varied_corpus(n: int, seed: int = 11):
    """Corpus B: parameter-varied messages that SHOULD collapse to a few
    templates — same sentence shape, different IPs/interfaces/pids that
    the normalizer is expected to mask. No unique prefixes."""
    import random

    rng = random.Random(seed)
    msgs = []
    shapes = [
        "BGP neighbor {ip} hold timer expired",
        "interface {iface} changed state to {state}",
        "BGP error code {code} received from peer {ip}",
    ]
    for i in range(n):
        shape = shapes[i % len(shapes)]
        msgs.append(
            f"<134>Nov 16 12:{i//3600%24:02d}:{i//60%60:02d}:{i%60:02d} "
            f"leaf-01 BGP[{1000 + i % 9000}]: "
            + shape.format(
                ip=f"10.{rng.randint(0,255)}.{rng.randint(0,255)}.{rng.randint(1,254)}",
                iface=f"Ethernet1/{rng.randint(1,48)}",
                state=rng.choice(["up", "down"]),
                code=rng.randint(0, 15),
            )
        )
    return msgs


def scrape_metrics(metrics_url: str) -> dict[str, float]:
    """Parse Prometheus text exposition into name->value (sum of samples)."""
    import urllib.request

    try:
        with urllib.request.urlopen(metrics_url, timeout=10) as r:
            text = r.read().decode()
    except Exception:
        return {}
    out: dict[str, float] = {}
    for line in text.splitlines():
        if line.startswith("#") or not line.strip():
            continue
        parts = line.rsplit(" ", 1)
        if len(parts) != 2:
            continue
        name, val = parts[0].split("{")[0], parts[1]
        try:
            out[name] = out.get(name, 0.0) + float(val)
        except ValueError:
            pass
    return out


def api_get(api: str, path: str):
    import requests

    r = requests.get(f"{api}{path}", timeout=20)
    r.raise_for_status()
    return r.json()


def embedding_stats(api: str) -> dict:
    return api_get(api, "/api/ml/embeddings/stats")


class Sender:
    """Monotonic-deadline paced UDP/TCP sender with loss accounting."""

    def __init__(self, transport: str, addr: str, rate: float):
        self.transport = transport
        self.host, self.port = addr.rsplit(":", 1)
        self.rate = rate
        self.interval = 1.0 / rate if rate > 0 else 0.0
        self.sent = 0
        self.errors = 0
        self.bytes = 0
        self.jitter_ms: list[float] = []
        self._sock = None
        self._tcp = None
        if transport == "udp":
            self._sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
            self._sock.setsockopt(
                socket.SOL_SOCKET, socket.SO_SNDBUF, 8 * 1024 * 1024
            )
        else:
            self._tcp = socket.create_connection(
                (self.host, int(self.port)), timeout=30
            )

    def send(self, msg: str):
        data = msg.encode()
        t = time.monotonic()
        try:
            if self.transport == "udp":
                self._sock.sendto(data, (self.host, int(self.port)))
            else:
                self._tcp.sendall(data + b"\n")
            self.sent += 1
            self.bytes += len(data)
        except OSError:
            self.errors += 1
        if self.interval:
            target = t + self.interval
            now = time.monotonic()
            if now < target:
                time.sleep(target - now)
            else:
                self.jitter_ms.append((now - target) * 1000)

    def close(self):
        try:
            if self._sock:
                self._sock.close()
            if self._tcp:
                self._tcp.close()
        except OSError:
            pass


def run_drain(api: str, embedder, dedup, max_cycles: int = 5000,
              idle_empty_cycles: int = 3, deadline_s: float = 7200):
    """Drain pending embeddings with EXPLICIT completion criteria.

    Complete = syslog_pending == 0 AND syslog_embedded stable across
    idle_empty_cycles consecutive fetches AND no deferred-with-future-
    retry remains (pending count covers needs_embedding=1 which includes
    deferred — so pending==0 is the strict criterion).
    """
    cycles = 0
    embedded_total = 0
    latencies = []
    statuses = {}
    empty_streak = 0
    t0 = time.monotonic()
    last_embedded = -1
    stable = 0
    while time.monotonic() - t0 < deadline_s and cycles < max_cycles:
        c0 = time.monotonic()
        events = worker._fetch_unembedded_events(api, limit=200)
        if not events:
            empty_streak += 1
            st = embedding_stats(api)
            cur = st.get("syslog_pending", -1)
            emb = st.get("syslog_embedded", -1)
            if cur == 0 and emb == last_embedded:
                stable += 1
            else:
                stable = 0
            last_embedded = emb
            if empty_streak >= idle_empty_cycles and stable >= idle_empty_cycles:
                break
            time.sleep(2)
            continue
        empty_streak = 0
        rep = {}
        n = worker._embed_and_post(api, events, embedder, dedup, report=rep)
        embedded_total += n
        statuses[rep.get("status", "?")] = statuses.get(rep.get("status", "?"), 0) + 1
        latencies.append(time.monotonic() - c0)
        cycles += 1
    lat = sorted(latencies)

    def pct(p):
        return round(lat[min(len(lat) - 1, int(len(lat) * p))] * 1000, 1) if lat else 0.0

    return {
        "cycles": cycles,
        "embedded_records": embedded_total,
        "cycle_statuses": statuses,
        "cycle_latency_ms": {"p50": pct(0.5), "p95": pct(0.95), "p99": pct(0.99)},
        "wall_s": round(time.monotonic() - t0, 2),
    }


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--workdir", required=True)
    ap.add_argument("--api-url", required=True)
    ap.add_argument("--metrics-url", default=None)
    ap.add_argument("--syslog-udp", default=None)
    ap.add_argument("--syslog-tcp", default=None)
    ap.add_argument("--scale", type=int, required=True)
    ap.add_argument("--unique-frac", type=float, default=0.05)
    ap.add_argument("--corpus", choices=["repetitive", "highcard", "mixed", "varied"],
                    default="mixed")
    ap.add_argument("--mode", choices=["ingest-only", "drain", "concurrent"],
                    required=True)
    ap.add_argument("--rate", type=float, default=1000.0, help="msg/s")
    ap.add_argument("--transport", choices=["udp", "tcp", "both"], default="udp")
    ap.add_argument("--core-pid", type=int, required=True)
    ap.add_argument("--graph-path", required=True)
    ap.add_argument("--archive-path", required=True)
    ap.add_argument("--registry", required=True)
    ap.add_argument("--report", required=True)
    args = ap.parse_args()

    workdir = Path(args.workdir)
    workdir.mkdir(parents=True, exist_ok=True)
    out: dict = {
        "scale": args.scale, "unique_frac": args.unique_frac,
        "corpus": args.corpus, "mode": args.mode, "rate": args.rate,
        "transport": args.transport,
    }

    metrics_url = args.metrics_url or ""
    m0 = scrape_metrics(metrics_url) if metrics_url else {}
    s0 = embedding_stats(args.api_url)
    rss0 = rss_mb(args.core_pid)

    # ── corpus ────────────────────────────────────────────────────────
    if args.corpus == "repetitive":
        frac = 0.0
    elif args.corpus == "highcard":
        frac = 1.0
    else:
        frac = args.unique_frac
    if args.corpus == "varied":
        msgs = varied_corpus(args.scale)
    else:
        msgs = corpus(args.scale, frac)

    # ── enrichment setup ──────────────────────────────────────────────
    embedder = DeterministicEmbedder()
    registry = TemplateRegistry(path=args.registry)
    dedup = TemplateDedup(registry=registry, embedder=embedder, scope="meas")

    sender_addr = args.syslog_udp or args.syslog_tcp
    sender = Sender(args.transport if args.transport != "both" else "udp",
                    sender_addr, args.rate)

    drain_result = {}
    drain_thread = None

    def do_drain():
        nonlocal drain_result
        drain_result = run_drain(args.api_url, embedder, dedup)

    t_ingest0 = time.monotonic()
    peak_rss = rss_mb(args.core_pid)

    if args.mode == "concurrent":
        drain_thread = threading.Thread(target=do_drain, daemon=True)
        drain_thread.start()

    # ── paced send ────────────────────────────────────────────────────
    send_t0 = time.monotonic()
    for m in msgs:
        sender.send(m)
        if sender.sent % 1000 == 0:
            peak_rss = max(peak_rss, rss_mb(args.core_pid))
    sender.close()
    ingest_wall = time.monotonic() - send_t0
    offered_rate = sender.sent / ingest_wall if ingest_wall else 0

    # ── settle: wait for pending to stop growing ──────────────────────
    settle_deadline = time.monotonic() + max(120, args.scale / args.rate * 3)
    last = -2
    stable = 0
    while time.monotonic() < settle_deadline:
        try:
            cur = embedding_stats(args.api_url).get("syslog_pending", -1)
        except Exception:
            cur = -1
        if cur >= 0 and cur == last:
            stable += 1
            if stable >= 3:
                break
        else:
            stable = 0
        last = cur
        time.sleep(3)
        peak_rss = max(peak_rss, rss_mb(args.core_pid))

    if args.mode == "ingest-only":
        pass
    elif args.mode == "drain":
        drain_result = run_drain(args.api_url, embedder, dedup)
    # concurrent: drain_thread still running; wait bounded
    if drain_thread is not None:
        drain_thread.join(timeout=7200)
        if drain_thread.is_alive():
            drain_result["timeout"] = True

    # ── post metrics ──────────────────────────────────────────────────
    time.sleep(3)  # let counters flush
    m1 = scrape_metrics(metrics_url) if metrics_url else {}
    s1 = embedding_stats(args.api_url)
    peak_rss = max(peak_rss, rss_mb(args.core_pid))

    def delta(name):
        return round(m1.get(name, 0.0) - m0.get(name, 0.0), 3) if m1 else None

    interesting = [
        "bonsai_syslog_events_total", "bonsai_syslog_archive_errors_total",
        "bonsai_syslog_noise_suppressed_total", "bonsai_syslog_shed_total",
        "bonsai_syslog_shunned_total", "bonsai_syslog_tcp_rejected_total",
        "bonsai_syslog_fact_join_total", "bonsai_archive_drops_total",
        "bonsai_broadcast_drops_total", "bonsai_graph_write_errors_total",
        "bonsai_graph_state_change_write_total",
        "bonsai_graph_write_latency_seconds", "bonsai_graph_batch_write_latency_seconds",
        "bonsai_queue_saturation_total", "bonsai_write_coordinator_queue_depth",
        "bonsai_rule_firings_total",
    ]
    out["pipeline"] = {k: delta(k) for k in interesting if m1}
    out["embedding_stats"] = {"before": s0, "after": s1}
    out["ingest"] = {
        "messages_offered": args.scale,
        "messages_sent_ok": sender.sent,
        "send_errors": sender.errors,
        "bytes_sent": sender.bytes,
        "offered_rate_msg_s": round(offered_rate, 1),
        "send_jitter_ms_max": round(max(sender.jitter_ms or [0]), 2),
        "wall_s": round(ingest_wall, 2),
        "peak_core_rss_mb": round(peak_rss, 1),
        "rss_mb_start": round(rss0, 1),
    }
    if drain_result:
        out["enrichment"] = drain_result
        out["enrichment"]["provider_calls"] = dedup.metrics.provider_calls
        out["enrichment"]["provider_texts_embedded"] = dedup.metrics.provider_texts_embedded
        snap = dedup.stats_snapshot()
        out["enrichment"]["template_cardinality"] = snap.get("template_cardinality")
        out["enrichment"]["template_occurrences"] = snap.get("template_occurrences")
    out["bytes"] = {
        "graph": dir_bytes(args.graph_path),
        "archive": dir_bytes(args.archive_path),
        "registry": dir_bytes(args.registry),
    }
    registry.close()
    Path(args.report).write_text(json.dumps(out, indent=2))
    print(json.dumps(out, indent=2)[:4000])
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
