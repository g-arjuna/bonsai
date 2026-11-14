"""Tests for WP6.5A persistent message-template dedup and embedding reuse.

Covers: three-stage lookup, template normalization safety (no collapse of
operationally significant differences), registry persistence across
restarts, concurrency claims, provider-failure handling, scope isolation,
model-revision re-embedding, and worker-level vector reuse.
"""
import hashlib
import threading

import numpy as np
import pytest

from bonsai_ml.message_templates import (
    NORMALIZATION_VERSION,
    MessageNormalizer,
    TemplateDedup,
    TemplateRegistry,
    _cache_key,
    build_template_dedup,
)


# ── fakes ─────────────────────────────────────────────────────────────────────


class FakeEmbedder:
    """Deterministic embedder: vector derived from text hash."""

    def __init__(self, dim: int = 8, fail: bool = False, bad_vectors: bool = False):
        self.model_name = "fake-model"
        self.model_revision = "rev1"
        self.dim = dim
        self.fail = fail
        self.bad_vectors = bad_vectors
        self.calls: list[list[str]] = []

    def embed_batch(self, texts):
        self.calls.append(list(texts))
        if self.fail:
            raise RuntimeError("provider unavailable")
        out = []
        for t in texts:
            if self.bad_vectors:
                out.append(np.zeros(self.dim, dtype=np.float32))
            else:
                digest = hashlib.sha256(t.encode()).digest()
                vals = [
                    (digest[i % len(digest)] - 128) / 128.0
                    for i in range(self.dim)
                ]
                arr = np.array(vals, dtype=np.float32)
                n = np.linalg.norm(arr)
                out.append(arr / n if n else arr)
        return np.array(out, dtype=np.float32)


def _make_dedup(tmp_path, embedder=None, scope="test-scope", name="reg.db", **kw):
    reg = TemplateRegistry(path=tmp_path / name, **{k: v for k, v in kw.items() if k == "now_ns"})
    return TemplateDedup(
        registry=reg,
        embedder=embedder or FakeEmbedder(),
        scope=scope,
        **{k: v for k, v in kw.items() if k != "now_ns"},
    )


# ── normalizer ────────────────────────────────────────────────────────────────


class TestMessageNormalizer:
    def test_masks_variable_fields(self):
        n = MessageNormalizer()
        t1, fp1 = n.template_fingerprint(
            "BGP neighbor 192.168.1.1 hold timer expired"
        )
        t2, fp2 = n.template_fingerprint(
            "BGP neighbor 10.0.0.254 hold timer expired"
        )
        assert t1 == t2
        assert fp1 == fp2
        assert "<VAR>" in t1

    def test_masks_interfaces_and_key_values(self):
        n = MessageNormalizer()
        t1, _ = n.template_fingerprint("interface Ethernet1/1 went down")
        t2, _ = n.template_fingerprint("interface Ethernet1/2 went down")
        assert t1 == t2
        t3, _ = n.template_fingerprint("peer=10.1.2.3 state=down")
        t4, _ = n.template_fingerprint("peer=10.9.9.9 state=down")
        assert t3 == t4
        assert "state=down" in t3  # non-variable kv preserved

    def test_preserves_operationally_significant_differences(self):
        n = MessageNormalizer()
        pairs = [
            ("interface Ethernet1/1 changed state to up",
             "interface Ethernet1/1 changed state to down"),
            ("BGP error code 1001 received from peer",
             "BGP error code 1002 received from peer"),
            ("CPU threshold 90 percent exceeded",
             "CPU threshold 95 percent exceeded"),
            ("admin shutdown issued by operator",
             "link failure detected on interface"),
        ]
        for a, b in pairs:
            _, fpa = n.template_fingerprint(a)
            _, fpb = n.template_fingerprint(b)
            assert fpa != fpb, f"collapsed: {a!r} vs {b!r}"

    def test_bare_numbers_are_never_masked(self):
        n = MessageNormalizer()
        t, _ = n.template_fingerprint("received 12345 packets on port 443")
        assert "12345" in t and "443" in t

    def test_pid_key_is_variable_but_state_is_not(self):
        n = MessageNormalizer()
        t1, _ = n.template_fingerprint("process pid=1234 restarted")
        t2, _ = n.template_fingerprint("process pid=9876 restarted")
        assert t1 == t2 and "pid=<VAR>" in t1

    def test_different_messages_get_different_templates(self):
        n = MessageNormalizer()
        _, fa = n.template_fingerprint("BGP session established with peer")
        _, fb = n.template_fingerprint("OSPF adjacency down on interface")
        assert fa != fb


# ── registry persistence ──────────────────────────────────────────────────────


class TestTemplateRegistry:
    def test_persists_across_reopen(self, tmp_path):
        path = tmp_path / "reg.db"
        reg1 = TemplateRegistry(path=path)
        key = _cache_key("s", "m", "r", NORMALIZATION_VERSION, "fp1")
        reg1.claim_or_get(
            scope="s", model_id="m", model_revision="r",
            norm_version=NORMALIZATION_VERSION, fingerprint="fp1",
            template_text="tpl", owner="o1",
        )
        reg1.complete_template(key, np.ones(4, dtype=np.float32), 4)
        reg1.close()

        reg2 = TemplateRegistry(path=path)
        state, row = reg2.claim_or_get(
            scope="s", model_id="m", model_revision="r",
            norm_version=NORMALIZATION_VERSION, fingerprint="fp1",
            template_text="tpl", owner="o2",
        )
        assert state == "ready"
        assert np.frombuffer(row["vector"], dtype=np.float32).shape == (4,)
        reg2.close()

    def test_claim_blocks_second_worker_until_expiry(self, tmp_path):
        now = [1_000_000_000]
        reg = TemplateRegistry(path=tmp_path / "reg.db", now_ns=lambda: now[0])
        kw = dict(
            scope="s", model_id="m", model_revision="r",
            norm_version=NORMALIZATION_VERSION, fingerprint="fpX",
            template_text="tpl",
        )
        assert reg.claim_or_get(owner="w1", **kw)[0] == "claimed"
        # Second worker sees pending while the claim is fresh.
        assert reg.claim_or_get(owner="w2", **kw)[0] == "pending"
        # After lease expiry the stale claim is reclaimable.
        now[0] += reg.claim_ttl_ns + 1
        assert reg.claim_or_get(owner="w2", **kw)[0] == "claimed"
        reg.close()

    def test_scope_isolation(self, tmp_path):
        reg = TemplateRegistry(path=tmp_path / "reg.db")
        kw = dict(
            model_id="m", model_revision="r",
            norm_version=NORMALIZATION_VERSION, fingerprint="fpS",
            template_text="tpl",
        )
        assert reg.claim_or_get(scope="tenant-a", owner="o", **kw)[0] == "claimed"
        state, _ = reg.claim_or_get(scope="tenant-b", owner="o", **kw)
        assert state == "claimed"  # independent row per scope
        reg.close()


# ── resolver: acceptance criteria ─────────────────────────────────────────────


class TestTemplateDedup:
    def test_10000_identical_messages_one_embedding(self, tmp_path):
        emb = FakeEmbedder()
        dedup = _make_dedup(tmp_path, emb)
        texts = ["interface Ethernet1/1 changed state to up"] * 10_000
        r1 = dedup.resolve_batch(texts)
        assert all(v is not None for v in r1)
        assert len(emb.calls) == 1 and len(emb.calls[0]) == 1

        # Second batch: pure cache hits, zero provider calls.
        emb.calls.clear()
        r2 = dedup.resolve_batch(texts)
        assert all(v is not None for v in r2)
        assert emb.calls == []
        stats = dedup.stats_snapshot()
        assert stats["provider_calls"] == 1
        assert stats["provider_texts_embedded"] == 1
        assert stats["exact_match_hits"] >= 5000
        assert stats["template_cardinality"] == 1
        # All occurrences share the identical vector.
        assert np.array_equal(r1[0], r2[-1])
        dedup.registry.close()

    def test_variable_fields_reuse_template_embedding(self, tmp_path):
        emb = FakeEmbedder()
        dedup = _make_dedup(tmp_path, emb)
        texts = [
            f"BGP neighbor 10.0.0.{i} hold timer expired" for i in range(1, 101)
        ]
        vecs = dedup.resolve_batch(texts)
        assert all(v is not None for v in vecs)
        assert len(emb.calls) == 1 and len(emb.calls[0]) == 1
        # Same template embedding reused for every occurrence.
        for v in vecs[1:]:
            assert np.array_equal(v, vecs[0])
        stats = dedup.stats_snapshot()
        assert stats["template_cardinality"] == 1
        assert stats["provider_calls_avoided"] >= 99
        dedup.registry.close()

    def test_semantically_different_messages_not_collapsed(self, tmp_path):
        emb = FakeEmbedder()
        dedup = _make_dedup(tmp_path, emb)
        texts = [
            "BGP neighbor 10.0.0.1 hold timer expired",
            "BGP neighbor 10.0.0.1 hold timer established",
            "BGP error code 1001 received from peer",
            "BGP error code 1002 received from peer",
            "interface Ethernet1/1 changed state to up",
            "interface Ethernet1/1 changed state to down",
        ]
        vecs = dedup.resolve_batch(texts)
        assert all(v is not None for v in vecs)
        # Each distinct template computed once: 6 unique templates here.
        assert len(emb.calls[0]) == len(set(emb.calls[0]))
        # Vectors for different templates must differ.
        assert not np.array_equal(vecs[0], vecs[1])
        assert not np.array_equal(vecs[2], vecs[3])
        assert not np.array_equal(vecs[4], vecs[5])
        dedup.registry.close()

    def test_reuse_survives_restart(self, tmp_path):
        path = tmp_path / "reg.db"
        emb = FakeEmbedder()
        reg1 = TemplateRegistry(path=path)
        d1 = TemplateDedup(registry=reg1, embedder=emb, scope="s")
        d1.resolve_batch(["BGP neighbor 10.0.0.1 hold timer expired"])
        assert len(emb.calls) == 1
        reg1.close()

        # "Restart": brand-new registry connection + dedup, same files.
        emb.calls.clear()
        reg2 = TemplateRegistry(path=path)
        d2 = TemplateDedup(registry=reg2, embedder=emb, scope="s")
        vecs = d2.resolve_batch(["BGP neighbor 10.0.0.2 hold timer expired"])
        assert vecs[0] is not None
        assert emb.calls == []  # no new provider call after restart
        reg2.close()

    def test_model_revision_change_triggers_reembedding(self, tmp_path):
        path = tmp_path / "reg.db"
        emb_v1 = FakeEmbedder()
        reg = TemplateRegistry(path=path)
        d1 = TemplateDedup(registry=reg, embedder=emb_v1, scope="s")
        d1.resolve_batch(["syslog template test message"])
        assert len(emb_v1.calls) == 1

        emb_v2 = FakeEmbedder()
        emb_v2.model_revision = "rev2"
        d2 = TemplateDedup(registry=reg, embedder=emb_v2, scope="s")
        vecs = d2.resolve_batch(["syslog template test message"])
        assert vecs[0] is not None
        assert len(emb_v2.calls) == 1  # controlled re-embedding
        # Old revision rows retained for audit.
        card = reg.cardinality(scope="s")
        assert card["template_cardinality"] == 2
        reg.close()

    def test_scope_isolation_no_cross_scope_reuse(self, tmp_path):
        emb = FakeEmbedder()
        path = tmp_path / "reg.db"
        reg_a = TemplateRegistry(path=path)
        da = TemplateDedup(registry=reg_a, embedder=emb, scope="tenant-a")
        da.resolve_batch(["same message text here"])
        assert len(emb.calls) == 1

        reg_b = TemplateRegistry(path=path)
        db_ = TemplateDedup(registry=reg_b, embedder=emb, scope="tenant-b")
        db_.resolve_batch(["same message text here"])
        assert len(emb.calls) == 2  # no reuse across scopes
        reg_a.close()
        reg_b.close()

    def test_provider_failure_does_not_poison_cache(self, tmp_path):
        emb = FakeEmbedder(fail=True)
        dedup = _make_dedup(tmp_path, emb)
        texts = ["provider failure test message one"]
        vecs = dedup.resolve_batch(texts)
        assert vecs == [None]
        assert len(emb.calls) == 1
        row_state, _ = dedup.registry.template_state(
            dedup.scope, dedup.model_id, dedup.model_revision,
            dedup.norm_version,
            dedup.normalizer.template_fingerprint(texts[0])[1],
        )
        assert row_state == "backoff"

        # Within backoff window: no new provider call.
        emb.calls.clear()
        vecs2 = dedup.resolve_batch(texts)
        assert vecs2 == [None]
        assert emb.calls == []

        # After backoff expires, retry succeeds and caches.
        emb.fail = False
        key = _cache_key(
            dedup.scope, dedup.model_id, dedup.model_revision,
            dedup.norm_version,
            dedup.normalizer.template_fingerprint(texts[0])[1],
        )
        row = dedup.registry.get_template(key)
        dedup.registry._conn.execute(
            "UPDATE templates SET next_retry_ns=0 WHERE cache_key=?", (key,)
        )
        dedup.registry._conn.commit()
        vecs3 = dedup.resolve_batch(texts)
        assert vecs3[0] is not None
        assert len(emb.calls) == 1
        dedup.registry.close()

    def test_invalid_vectors_rejected(self, tmp_path):
        emb = FakeEmbedder(bad_vectors=True)
        dedup = _make_dedup(tmp_path, emb)
        vecs = dedup.resolve_batch(["invalid vector test message"])
        assert vecs == [None]
        stats = dedup.stats_snapshot()
        assert stats["invalid_vectors"] == 1
        assert stats["templates_embedded"] == 0
        dedup.registry.close()

    def test_pending_template_not_double_embedded(self, tmp_path):
        """A second worker must not embed a template claimed by the first."""
        path = tmp_path / "reg.db"
        emb1 = FakeEmbedder()
        reg1 = TemplateRegistry(path=path)
        d1 = TemplateDedup(registry=reg1, embedder=emb1, scope="s")

        # Worker 1 claims the template but has not completed it.
        tpl_text, tpl_fp = d1.normalizer.template_fingerprint(
            "concurrent claim test message"
        )
        state, _ = reg1.claim_or_get(
            scope="s", model_id=d1.model_id, model_revision=d1.model_revision,
            norm_version=d1.norm_version, fingerprint=tpl_fp,
            template_text=tpl_text, owner="other-worker",
        )
        assert state == "claimed"

        # Worker 2 sees pending and does not call the provider.
        emb2 = FakeEmbedder()
        reg2 = TemplateRegistry(path=path)
        d2 = TemplateDedup(registry=reg2, embedder=emb2, scope="s")
        vecs = d2.resolve_batch(["concurrent claim test message"])
        assert vecs == [None]
        assert emb2.calls == []
        reg1.close()
        reg2.close()

    def test_dedup_disabled_path_untouched(self, tmp_path):
        cfg = {"dedup_enabled": False}
        emb = FakeEmbedder()
        assert build_template_dedup(cfg, emb) is None

    def test_empty_and_missing_text(self, tmp_path):
        dedup = _make_dedup(tmp_path)
        vecs = dedup.resolve_batch(["", "   "])
        assert all(v is None for v in vecs)
        dedup.registry.close()


# ── worker integration ────────────────────────────────────────────────────────


class TestWorkerIntegration:
    def test_embed_and_post_reuses_vectors(self, tmp_path, monkeypatch):
        import bonsai_ml.syslog_embedding_worker as w

        posted = []

        class Resp:
            ok = True
            status_code = 200

            def raise_for_status(self):
                pass

            def json(self):
                return {}

        monkeypatch.setattr(
            "requests.post",
            lambda url, json=None, headers=None, timeout=None: (
                posted.append(json) or Resp()
            ),
        )

        emb = FakeEmbedder()
        dedup = _make_dedup(tmp_path, emb)
        events = [
            {"id": f"e{i}", "detail_json": '{"message": "BGP hold timer expired for 10.0.0.%d"}' % i}
            for i in range(1, 6)
        ]
        n1 = w._embed_and_post("http://x", events, emb, dedup)
        assert n1 == 5
        assert len(emb.calls) == 1 and len(emb.calls[0]) == 1  # one template
        assert len(posted[0]["embeddings"]) == 5  # per-event records kept

        # Identical second batch: cache hits only, still 5 records posted.
        n2 = w._embed_and_post("http://x", events, emb, dedup)
        assert n2 == 5
        assert len(emb.calls) == 1  # no additional provider calls
        vecs1 = [r["vector"] for r in posted[0]["embeddings"]]
        vecs2 = [r["vector"] for r in posted[1]["embeddings"]]
        assert vecs1 == vecs2
        assert all("model_revision" in r for r in posted[0]["embeddings"])
        dedup.registry.close()

    def test_unresolved_events_not_posted(self, tmp_path, monkeypatch):
        import bonsai_ml.syslog_embedding_worker as w

        posted = []

        class Resp:
            ok = True
            status_code = 200

            def raise_for_status(self):
                pass

        monkeypatch.setattr(
            "requests.post",
            lambda url, json=None, headers=None, timeout=None: (
                posted.append(json) or Resp()
            ),
        )

        emb = FakeEmbedder(fail=True)
        dedup = _make_dedup(tmp_path, emb)
        events = [{"id": "e1", "message": "some syslog message"}]
        n = w._embed_and_post("http://x", events, emb, dedup)
        assert n == 0
        assert posted == []  # provider failure -> nothing marked embedded
        dedup.registry.close()
