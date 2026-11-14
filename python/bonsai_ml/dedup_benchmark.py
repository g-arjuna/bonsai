"""WP6.5A benchmark: per-event embedding vs persistent template dedup.

Generates a synthetic syslog corpus with configurable repetition/uniqueness
and measures provider work under two strategies:

  baseline — legacy per-event ``embed_batch`` over every message.
  dedup    — :class:`TemplateDedup` three-stage resolution.

Usage:
    python -m bonsai_ml.dedup_benchmark --messages 10000 --unique-frac 0.05
    python -m bonsai_ml.dedup_benchmark --messages 10000 --embedder hash

``--embedder hash`` (default) uses a deterministic local hash embedder so the
benchmark measures *provider-call reduction* without a model download.
``--embedder st`` uses sentence-transformers when installed and the model is
cached; it reports model load time separately.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import logging
import tempfile
import time
from pathlib import Path

import numpy as np

log = logging.getLogger(__name__)


class HashEmbedder:
    """Deterministic stand-in provider: vector = normalized sha256 stream."""

    def __init__(self, dim: int = 384):
        self.model_name = "hash-bench"
        self.model_revision = "1"
        self.dim = dim
        self.calls: list[list[str]] = []
        self.texts_embedded = 0

    def embed_batch(self, texts):
        self.calls.append(list(texts))
        self.texts_embedded += len(texts)
        out = np.zeros((len(texts), self.dim), dtype=np.float32)
        n_chunks = (self.dim + 31) // 32
        for i, t in enumerate(texts):
            stream = b"".join(
                hashlib.sha256(f"{t}:{j}".encode()).digest()
                for j in range(n_chunks)
            )
            rng = (
                np.frombuffer(stream, dtype=np.uint8)[: self.dim]
                .astype(np.float32)
                / 255.0
            )
            n = np.linalg.norm(rng)
            out[i] = rng / n if n else rng
        return out


def _corpus(n_messages: int, unique_frac: float, seed: int = 7) -> list[str]:
    """Synthetic syslog corpus: repetitive variable-field messages + uniques."""
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
    ips = [f"10.{rng.randint(0,255)}.{rng.randint(0,255)}.{rng.randint(1,254)}"
           for _ in range(50)]
    ifaces = [f"Ethernet1/{i}" for i in range(1, 25)] + [
        f"Gi0/{i}" for i in range(1, 13)
    ]
    codes = [1001, 1002, 1003, 1004, 1005, 2001, 2002]
    sevs = ["warning", "error", "critical"]
    msgs: list[str] = []
    n_unique = int(n_messages * unique_frac)
    for i in range(n_messages - n_unique):
        t = rng.choice(templates)
        msg = (
            t.replace("{ip}", rng.choice(ips))
            .replace("{iface}", rng.choice(ifaces))
            .replace("{state}", rng.choice(states))
            .replace("{code}", str(rng.choice(codes)))
            .replace("{rid}", f"{rng.randint(1,65535)}.{rng.randint(0,65535)}.{rng.randint(0,65535)}.{rng.randint(1,254)}")
            .replace("{oid}", f"1.3.6.1.4.1.{rng.randint(1000,9999)}.{rng.randint(1,99)}")
            .replace("{sev}", rng.choice(sevs))
        )
        msgs.append(msg)
    for i in range(n_unique):
        msgs.append(f"novel diagnostic message {i} at {time.time_ns()} on host-{i}")
    rng.shuffle(msgs)
    return msgs


def _batch_embed(embedder, texts: list[str], batch: int = 512) -> int:
    """Legacy per-event path: every text costs one provider input."""
    n = 0
    for i in range(0, len(texts), batch):
        embedder.embed_batch(texts[i : i + batch])
        n += len(texts[i : i + batch])
    return n


def run_benchmark(
    n_messages: int = 10_000,
    unique_frac: float = 0.05,
    embedder_kind: str = "hash",
    batch_size: int = 200,
) -> dict:
    from bonsai_ml.message_templates import TemplateDedup, TemplateRegistry

    texts = _corpus(n_messages, unique_frac)

    if embedder_kind == "st":
        from bonsai_ml.text_embeddings import EmbeddingConfig, TextEmbedder

        cfg = EmbeddingConfig(model_name="unsloth/bge-small-en-v1.5", dim=384)
        embedder = TextEmbedder(cfg)
    else:
        embedder = HashEmbedder()

    result: dict = {"messages": n_messages, "unique_frac": unique_frac}

    # ── baseline: per-event embedding ──────────────────────────────────────
    t0 = time.monotonic()
    baseline_texts = _batch_embed(embedder, texts)
    baseline_s = time.monotonic() - t0
    result["baseline"] = {
        "provider_texts": baseline_texts,
        "wall_s": round(baseline_s, 3),
        "texts_per_sec": round(baseline_texts / baseline_s, 1) if baseline_s else None,
    }

    # ── dedup path ─────────────────────────────────────────────────────────
    with tempfile.TemporaryDirectory() as td:
        reg = TemplateRegistry(path=Path(td) / "tpl.db")
        dedup = TemplateDedup(registry=reg, embedder=embedder, scope="bench")
        t0 = time.monotonic()
        resolved = 0
        for i in range(0, len(texts), batch_size):
            vecs = dedup.resolve_batch(texts[i : i + batch_size])
            resolved += sum(1 for v in vecs if v is not None)
        dedup_s = time.monotonic() - t0
        snap = dedup.stats_snapshot()
        reg.close()

    result["dedup"] = {
        "provider_calls": snap["provider_calls"],
        "provider_texts_embedded": snap["provider_texts_embedded"],
        "events_resolved": resolved,
        "provider_calls_avoided": snap["provider_calls_avoided"],
        "embedding_cache_hit_ratio": snap["embedding_cache_hit_ratio"],
        "template_cardinality": snap["template_cardinality"],
        "template_occurrences": snap["template_occurrences"],
        "wall_s": round(dedup_s, 3),
        "texts_per_sec": round(resolved / dedup_s, 1) if dedup_s else None,
        "estimated_provider_ms_saved": snap["estimated_provider_ms_saved"],
    }
    result["provider_text_reduction"] = round(
        1 - snap["provider_texts_embedded"] / max(1, baseline_texts), 4
    )
    return result


def main() -> None:
    logging.basicConfig(level=logging.INFO)
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--messages", type=int, default=10_000)
    ap.add_argument("--unique-frac", type=float, default=0.05)
    ap.add_argument("--embedder", choices=["hash", "st"], default="hash")
    ap.add_argument("--batch-size", type=int, default=200)
    ap.add_argument("--json-out", default="")
    args = ap.parse_args()

    result = run_benchmark(
        n_messages=args.messages,
        unique_frac=args.unique_frac,
        embedder_kind=args.embedder,
        batch_size=args.batch_size,
    )
    print(json.dumps(result, indent=2))
    if args.json_out:
        Path(args.json_out).write_text(json.dumps(result, indent=2) + "\n")


if __name__ == "__main__":
    main()
