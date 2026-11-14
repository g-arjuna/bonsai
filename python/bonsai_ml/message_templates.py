"""Persistent semantic message-template recognition and embedding reuse (WP6.5A).

Local-first, three-stage deduplication for text embeddings:

  1. Exact message fingerprint lookup (sha256 of the raw message text).
  2. Normalized message-template fingerprint lookup (sha256 of the template
     produced by :class:`MessageNormalizer`).
  3. New embedding computation — only for templates absent from the registry.

Template mining evaluation
--------------------------
Established log-template miners were evaluated before selecting an approach:

* **Drain / Drain3** (``pip install drain3``): proven on public log datasets,
  but it is an *additional runtime dependency* that is not vendored in this
  repository, is unavailable in the offline build environment, and merges
  templates by token-similarity thresholds that can collapse operationally
  distinct messages (e.g. different error codes or thresholds that differ in a
  single token).  Its similarity clustering also makes the template boundary
  dependent on insertion order and cluster history, which is undesirable for
  a security-relevant, versioned cache key.
* **Vendor-specific parsers** (TextFSM/pyATS pipelines under ``src/parser_chain.rs``
  and SuzieQ-derived schemas): they target structured device *command* output,
  not free-form syslog text, and carry per-NOS-template maintenance cost.
* **Selected approach**: a conservative, deterministic, token-level
  normalizer implemented in this module.  Only unambiguous variable fields
  (IP addresses, MAC/UUID/hex identifiers, ISO timestamps, common interface
  names, identifier-style ``key=value`` pairs) are masked.  Bare numbers,
  state keywords (``up``/``down``/``established``), error codes, thresholds
  and administrative-vs-failure wording always remain literal, so semantically
  different messages cannot be collapsed into one template.  The normalizer
  is pure, versioned (``NORMALIZATION_VERSION``) and has no external
  dependency.

Persistence and scope
---------------------
Templates and exact-message mappings live in a SQLite registry (default
``runtime/message_templates.db``, override with ``BONSAI_TEMPLATE_REGISTRY_PATH``
or the ``template_registry_path`` embedding-config key).  The registry is
schema-versioned (``PRAGMA user_version``) and survives Collector, Core and
BonPy restarts because it is local to the embedding worker.

Every cache key includes ``(scope, model_id, model_revision,
normalization_version, fingerprint)``.  ``scope`` (``dedup_scope`` config or
``BONSAI_EMBEDDING_SCOPE``, default ``"local"``) isolates tenants/deployments:
embeddings are never reused across scopes, which keeps the reuse boundary
explicit for authorization and data-sharing policy.  A model or normalization
upgrade produces new keys, so re-embedding is controlled and old vectors are
retained for audit rather than silently overwritten.

Concurrency
-----------
New templates are claimed in the registry with a bounded lease
(``claim_ttl_secs``).  Concurrent workers (threads or processes) cannot
embed the same new template twice: the claim is an atomic SQLite
``BEGIN IMMEDIATE`` transaction; losers observe ``pending`` and leave the
event for a later cycle.  Stale claims are reclaimable after the lease
expires, so a crashed worker cannot wedge a template forever.

Provider failures never poison the cache: a vector is stored only after
validation (dimensionality, finite values, non-zero norm, expected count).
Failed templates are recorded with ``status='failed'``, a ``fail_count`` and
a ``next_retry_ns`` backoff; the affected events stay ``needs_embedding`` and
are retried on a later cycle.
"""
from __future__ import annotations

import hashlib
import logging
import os
import re
import sqlite3
import threading
import time
import uuid
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Callable, Optional

import numpy as np

from .memory_manager import _LruCache

log = logging.getLogger(__name__)

NORMALIZATION_VERSION = "syslog-tpl-v1"
SCHEMA_VERSION = 1
DEFAULT_REGISTRY_PATH = "runtime/message_templates.db"
DEFAULT_SCOPE = "local"
DEFAULT_CLAIM_TTL_SECS = 600
DEFAULT_LRU_ENTRIES = 10_000
MAX_TEMPLATE_TEXT = 1024
MAX_MESSAGE_CHARS = 8192
MAX_ERROR_LEN = 200

# Backoff schedule (seconds) for failed template embeddings.
_FAIL_BACKOFF_SECS = (60, 300, 900, 3600)


# ── Normalization ─────────────────────────────────────────────────────────────

# Unambiguous variable-token patterns.  Anything not matched stays literal so
# operationally significant words (up/down, error codes, thresholds, admin vs
# failure) are never normalized away.
_RE_IPV4 = re.compile(r"^(?:\d{1,3}\.){3}\d{1,3}(?::\d{1,5})?$")
_RE_MAC_DASH = re.compile(r"^(?:[0-9a-fA-F]{2}-){5}[0-9a-fA-F]{2}$")
_RE_UUID = re.compile(
    r"^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-"
    r"[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$"
)
_RE_ISO_TS = re.compile(
    r"^\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:?\d{2})?$"
)
# Colon-separated hex tokens: IPv6, colon MACs, HH:MM:SS times.
_RE_COLON_HEX = re.compile(r"^[0-9a-fA-F]*:[0-9a-fA-F:]+$")
# Long hex blobs / prefixed hex that contain at least one a-f letter, so pure
# decimal numbers (error codes, thresholds, counts) are never masked.
_RE_HEX_BLOB = re.compile(r"^(?:0x)?(?=[0-9a-fA-F]*[a-fA-F])[0-9a-fA-F]{8,}$")
# Interface-style names: letters followed by digits plus a separator group
# (Ethernet1/1, Gi0/2, xe-0/0/1.100, HundredGigE1/0/1) or a known ifname prefix
# with a digit (eth0, ens192, et-0/0/1).
_RE_IFACE_SEP = re.compile(r"^[A-Za-z][A-Za-z0-9._-]*\d+(?:[./:-][0-9A-Za-z._-]+)+$")
_RE_IFACE_PREFIX = re.compile(
    r"^(?:eth|ens|en|em|eno|enp|enx|wl|wlan|xe|et|ge|gi|te|fa|fxp|bme|"
    r"bond|vlan|swp|p|cfe|cf|so|fe)[-/.:]?\d",
    re.IGNORECASE,
)
_RE_KV = re.compile(r"^([^=\s]{1,64})=(\S{1,256})$")

# key=value keys whose values are identifiers, not semantics.  Values under
# these keys are masked; all other key=value pairs stay literal unless the
# value itself matches a variable pattern.
_VAR_VALUE_KEYS = frozenset(
    {
        "pid", "ppid", "fd", "tid", "task", "seq", "sequence", "msgid",
        "mid", "boot_id", "session", "handle", "inode", "generation",
    }
)

_VAR_TOKEN = "<VAR>"


def _is_variable_value(value: str) -> bool:
    """True when a bare token or key=value value is an unambiguous variable."""
    return bool(
        _RE_IPV4.match(value)
        or _RE_MAC_DASH.match(value)
        or _RE_UUID.match(value)
        or _RE_ISO_TS.match(value)
        or _RE_COLON_HEX.match(value)
        or _RE_HEX_BLOB.match(value)
        or _RE_IFACE_SEP.match(value)
        or _RE_IFACE_PREFIX.match(value)
    )


class MessageNormalizer:
    """Deterministic syslog message-template normalizer (versioned).

    ``normalize(text)`` returns the template text: variable tokens are
    replaced by ``<VAR>``; everything else is preserved verbatim (modulo
    whitespace collapsing).  ``fingerprint`` hashes the raw message for the
    exact-match stage; ``template_fingerprint`` hashes the template.
    """

    version = NORMALIZATION_VERSION

    def fingerprint(self, text: str) -> str:
        return hashlib.sha256(text.encode("utf-8", "replace")).hexdigest()

    def normalize(self, text: str) -> str:
        if len(text) > MAX_MESSAGE_CHARS:
            text = text[:MAX_MESSAGE_CHARS]
        return " ".join(self._normalize_token(t) for t in text.split())

    def template_fingerprint(self, text: str) -> tuple[str, str]:
        """Return (template_text, template_fingerprint) for a raw message."""
        template = self.normalize(text)
        fp = hashlib.sha256(template.encode("utf-8", "replace")).hexdigest()
        return template, fp

    def _normalize_token(self, tok: str) -> str:
        if _is_variable_value(tok):
            return _VAR_TOKEN
        m = _RE_KV.match(tok)
        if m:
            key, value = m.group(1), m.group(2)
            if key.lower() in _VAR_VALUE_KEYS or _is_variable_value(value):
                return f"{key}={_VAR_TOKEN}"
        return tok


# ── Metrics ───────────────────────────────────────────────────────────────────


@dataclass
class DedupMetrics:
    """Counters for the three-stage dedup pipeline."""

    events_seen: int = 0
    exact_hits: int = 0
    template_hits: int = 0
    new_templates_claimed: int = 0
    templates_embedded: int = 0
    provider_texts_embedded: int = 0
    provider_calls: int = 0
    provider_failures: int = 0
    pending_skips: int = 0
    backoff_skips: int = 0
    invalid_vectors: int = 0
    embed_latency_ms_total: float = 0.0
    embed_texts_total: int = 0
    events_posted: int = 0
    events_resolved: int = 0

    def snapshot(self, registry: Optional["TemplateRegistry"] = None) -> dict[str, Any]:
        lookups = (
            self.exact_hits + self.template_hits + self.new_templates_claimed
        )
        # Under the legacy per-event approach every resolved event costs one
        # provider text.  Dedup computes only unique new templates, so the
        # avoided calls are resolved events minus actual provider texts.
        calls_avoided = max(0, self.events_resolved - self.provider_texts_embedded)
        avg_ms = (
            self.embed_latency_ms_total / self.embed_texts_total
            if self.embed_texts_total
            else 0.0
        )
        snap: dict[str, Any] = {
            "events_seen": self.events_seen,
            "exact_match_hits": self.exact_hits,
            "template_match_hits": self.template_hits,
            "new_templates": self.new_templates_claimed,
            "templates_embedded": self.templates_embedded,
            "provider_calls": self.provider_calls,
            "provider_texts_embedded": self.provider_texts_embedded,
            "provider_calls_avoided": calls_avoided,
            "pending_skips": self.pending_skips,
            "backoff_skips": self.backoff_skips,
            "invalid_vectors": self.invalid_vectors,
            "provider_failures": self.provider_failures,
            "embedding_cache_hit_ratio": (
                round((self.exact_hits + self.template_hits) / lookups, 4)
                if lookups
                else 0.0
            ),
            "estimated_provider_ms_saved": round(calls_avoided * avg_ms, 1),
            "events_resolved": self.events_resolved,
            "events_posted": self.events_posted,
        }
        if registry is not None:
            snap.update(registry.cardinality())
        return snap


# ── Registry ──────────────────────────────────────────────────────────────────

_SCHEMA_SQL = """
CREATE TABLE IF NOT EXISTS templates (
    cache_key TEXT PRIMARY KEY,
    scope TEXT NOT NULL,
    model_id TEXT NOT NULL,
    model_revision TEXT NOT NULL,
    normalization_version TEXT NOT NULL,
    fingerprint TEXT NOT NULL,
    template_text TEXT NOT NULL,
    vector BLOB,
    dim INTEGER NOT NULL DEFAULT 0,
    status TEXT NOT NULL,
    occurrence_count INTEGER NOT NULL DEFAULT 0,
    fail_count INTEGER NOT NULL DEFAULT 0,
    first_seen_ns INTEGER NOT NULL,
    last_seen_ns INTEGER NOT NULL,
    claim_owner TEXT,
    claimed_at_ns INTEGER,
    claim_expires_ns INTEGER,
    next_retry_ns INTEGER,
    last_error TEXT
);
CREATE INDEX IF NOT EXISTS idx_tpl_model
    ON templates(model_id, model_revision, status);
CREATE INDEX IF NOT EXISTS idx_tpl_scope
    ON templates(scope, model_id, model_revision, normalization_version);

CREATE TABLE IF NOT EXISTS exact_map (
    scope TEXT NOT NULL,
    model_id TEXT NOT NULL,
    model_revision TEXT NOT NULL,
    normalization_version TEXT NOT NULL,
    exact_fp TEXT NOT NULL,
    cache_key TEXT NOT NULL,
    last_seen_ns INTEGER NOT NULL,
    PRIMARY KEY (scope, model_id, model_revision,
                 normalization_version, exact_fp)
);
CREATE INDEX IF NOT EXISTS idx_exact_key ON exact_map(cache_key);
"""

_TEMPLATE_COLS = (
    "cache_key", "scope", "model_id", "model_revision",
    "normalization_version", "fingerprint", "template_text", "vector",
    "dim", "status", "occurrence_count", "fail_count", "first_seen_ns",
    "last_seen_ns", "claim_owner", "claimed_at_ns", "claim_expires_ns",
    "next_retry_ns", "last_error",
)


def _cache_key(
    scope: str,
    model_id: str,
    model_revision: str,
    norm_version: str,
    fingerprint: str,
) -> str:
    raw = "|".join(
        (scope, model_id, model_revision, norm_version, fingerprint)
    ).encode("utf-8")
    return hashlib.sha256(raw).hexdigest()


class TemplateRegistry:
    """Persistent, versioned message-template registry backed by SQLite.

    Args:
        path: SQLite file path (``":memory:"`` supported for tests).
        claim_ttl_ns: lease duration for in-flight template embeddings.
        now_ns: injectable clock for tests.
    """

    def __init__(
        self,
        path: str | Path = DEFAULT_REGISTRY_PATH,
        claim_ttl_ns: int = DEFAULT_CLAIM_TTL_SECS * 1_000_000_000,
        now_ns: Optional[Callable[[], int]] = None,
    ) -> None:
        self.path = str(path)
        self.claim_ttl_ns = claim_ttl_ns
        self._now_ns = now_ns or time.time_ns
        self._lock = threading.RLock()
        if self.path != ":memory:":
            Path(self.path).parent.mkdir(parents=True, exist_ok=True)
        self._conn = sqlite3.connect(
            self.path, check_same_thread=False, timeout=30.0
        )
        self._conn.execute("PRAGMA journal_mode=WAL")
        self._conn.execute("PRAGMA synchronous=NORMAL")
        self._conn.execute("PRAGMA busy_timeout=30000")
        self._init_schema()

    def _init_schema(self) -> None:
        with self._lock:
            version = self._conn.execute("PRAGMA user_version").fetchone()[0]
            if version > SCHEMA_VERSION:
                raise RuntimeError(
                    f"message template registry schema v{version} is newer "
                    f"than supported v{SCHEMA_VERSION}: {self.path}"
                )
            self._conn.executescript(_SCHEMA_SQL)
            self._conn.execute(f"PRAGMA user_version={SCHEMA_VERSION}")
            self._conn.commit()

    def close(self) -> None:
        with self._lock:
            self._conn.close()

    # ── reads ──────────────────────────────────────────────────────────────

    def exact_lookup(
        self,
        scope: str,
        model_id: str,
        model_revision: str,
        norm_version: str,
        exact_fp: str,
    ) -> Optional[str]:
        """Return the template cache_key for an exact message, if known."""
        with self._lock:
            row = self._conn.execute(
                "SELECT cache_key FROM exact_map WHERE scope=? AND model_id=? "
                "AND model_revision=? AND normalization_version=? AND exact_fp=?",
                (scope, model_id, model_revision, norm_version, exact_fp),
            ).fetchone()
        return row[0] if row else None

    def get_template(self, cache_key: str) -> Optional[dict[str, Any]]:
        """Return a template row (vector as bytes) or None."""
        with self._lock:
            row = self._conn.execute(
                f"SELECT {', '.join(_TEMPLATE_COLS)} FROM templates "
                "WHERE cache_key=?",
                (cache_key,),
            ).fetchone()
        if row is None:
            return None
        return dict(zip(_TEMPLATE_COLS, row))

    def template_state(
        self,
        scope: str,
        model_id: str,
        model_revision: str,
        norm_version: str,
        fingerprint: str,
    ) -> tuple[str, Optional[dict[str, Any]]]:
        """Classify a template without claiming it.

        Returns ``(state, row)`` where state is ``ready``, ``pending``,
        ``backoff`` or ``new`` (absent, stale-claimed, or retry-due).
        """
        key = _cache_key(scope, model_id, model_revision, norm_version, fingerprint)
        row = self.get_template(key)
        if row is None:
            return "new", None
        now = self._now_ns()
        if row["status"] == "ready" and row["vector"] is not None:
            return "ready", row
        if row["status"] == "pending" and (row["claim_expires_ns"] or 0) > now:
            return "pending", row
        if row["status"] == "failed" and (row["next_retry_ns"] or 0) > now:
            return "backoff", row
        return "new", row

    # ── claims / completion ────────────────────────────────────────────────

    def claim_or_get(
        self,
        *,
        scope: str,
        model_id: str,
        model_revision: str,
        norm_version: str,
        fingerprint: str,
        template_text: str,
        owner: str,
    ) -> tuple[str, Optional[dict[str, Any]]]:
        """Atomically claim a template for embedding or report its state.

        Returns ``(state, row)`` where state is one of:
          ``claimed``  — caller owns the embedding computation for this template.
          ``ready``    — row contains a usable vector.
          ``pending``  — another worker holds a live claim; retry later.
          ``backoff``  — last attempt failed; retry after ``next_retry_ns``.
        """
        key = _cache_key(scope, model_id, model_revision, norm_version, fingerprint)
        now = self._now_ns()
        expires = now + self.claim_ttl_ns
        with self._lock:
            self._conn.execute("BEGIN IMMEDIATE")
            try:
                row = self._conn.execute(
                    "SELECT status, vector, dim, claim_expires_ns, next_retry_ns "
                    "FROM templates WHERE cache_key=?",
                    (key,),
                ).fetchone()
                if row is None:
                    self._conn.execute(
                        "INSERT INTO templates (cache_key, scope, model_id, "
                        "model_revision, normalization_version, fingerprint, "
                        "template_text, status, first_seen_ns, last_seen_ns, "
                        "claim_owner, claimed_at_ns, claim_expires_ns) "
                        "VALUES (?,?,?,?,?,?,?,'pending',?,?,?,?,?)",
                        (
                            key, scope, model_id, model_revision, norm_version,
                            fingerprint, template_text[:MAX_TEMPLATE_TEXT],
                            now, now, owner, now, expires,
                        ),
                    )
                    self._conn.execute("COMMIT")
                    return "claimed", None

                status, vector, dim, claim_exp, next_retry = row
                if status == "ready" and vector is not None:
                    self._conn.execute("COMMIT")
                    return "ready", {"vector": vector, "dim": dim}
                if status == "pending" and (claim_exp or 0) > now:
                    self._conn.execute("COMMIT")
                    return "pending", None
                if status == "failed" and (next_retry or 0) > now:
                    self._conn.execute("COMMIT")
                    return "backoff", None

                # Stale claim or retry-due failure: (re)claim.
                self._conn.execute(
                    "UPDATE templates SET status='pending', claim_owner=?, "
                    "claimed_at_ns=?, claim_expires_ns=?, last_error=NULL "
                    "WHERE cache_key=?",
                    (owner, now, expires, key),
                )
                self._conn.execute("COMMIT")
                return "claimed", None
            except Exception:
                self._conn.execute("ROLLBACK")
                raise

    def complete_template(
        self, cache_key: str, vector: np.ndarray, dim: int
    ) -> None:
        """Mark a claimed template ready with its validated vector."""
        now = self._now_ns()
        blob = np.ascontiguousarray(vector, dtype=np.float32).tobytes()
        with self._lock:
            self._conn.execute(
                "UPDATE templates SET status='ready', vector=?, dim=?, "
                "claim_owner=NULL, claimed_at_ns=NULL, claim_expires_ns=NULL, "
                "next_retry_ns=NULL, last_error=NULL, last_seen_ns=? "
                "WHERE cache_key=?",
                (blob, dim, now, cache_key),
            )
            self._conn.commit()

    def fail_template(self, cache_key: str, error: str) -> None:
        """Record a retryable provider failure; never stores a vector."""
        now = self._now_ns()
        with self._lock:
            row = self._conn.execute(
                "SELECT fail_count FROM templates WHERE cache_key=?",
                (cache_key,),
            ).fetchone()
            fail_count = (row[0] if row else 0) + 1
            backoff = _FAIL_BACKOFF_SECS[
                min(fail_count - 1, len(_FAIL_BACKOFF_SECS) - 1)
            ]
            self._conn.execute(
                "UPDATE templates SET status='failed', fail_count=?, "
                "next_retry_ns=?, last_error=?, claim_owner=NULL, "
                "claimed_at_ns=NULL, claim_expires_ns=NULL WHERE cache_key=?",
                (
                    fail_count,
                    now + backoff * 1_000_000_000,
                    error[:MAX_ERROR_LEN],
                    cache_key,
                ),
            )
            self._conn.commit()

    # ── occurrence bookkeeping ─────────────────────────────────────────────

    def store_exact_mappings(
        self, mappings: list[tuple[str, str, str, str, str, str, int]]
    ) -> None:
        """Upsert (scope, model, rev, norm, exact_fp) -> cache_key rows.

        Each tuple: (scope, model_id, model_revision, norm_version,
        exact_fp, cache_key, now_ns).
        """
        if not mappings:
            return
        with self._lock:
            self._conn.executemany(
                "INSERT INTO exact_map (scope, model_id, model_revision, "
                "normalization_version, exact_fp, cache_key, last_seen_ns) "
                "VALUES (?,?,?,?,?,?,?) "
                "ON CONFLICT(scope, model_id, model_revision, "
                "normalization_version, exact_fp) "
                "DO UPDATE SET cache_key=excluded.cache_key, "
                "last_seen_ns=excluded.last_seen_ns",
                mappings,
            )
            self._conn.commit()

    def bump_occurrences(
        self, cache_keys: list[str], now_ns: Optional[int] = None
    ) -> None:
        """Increment occurrence_count/last_seen for resolved templates."""
        if not cache_keys:
            return
        now = now_ns if now_ns is not None else self._now_ns()
        counts: dict[str, int] = {}
        for k in cache_keys:
            counts[k] = counts.get(k, 0) + 1
        with self._lock:
            self._conn.executemany(
                "UPDATE templates SET occurrence_count=occurrence_count+?, "
                "last_seen_ns=? WHERE cache_key=?",
                [(n, now, k) for k, n in counts.items()],
            )
            self._conn.commit()

    def cardinality(
        self,
        scope: Optional[str] = None,
        model_id: Optional[str] = None,
        model_revision: Optional[str] = None,
    ) -> dict[str, Any]:
        """Template counts by status (optionally scoped to a model)."""
        clauses: list[str] = []
        params: list[str] = []
        if scope is not None:
            clauses.append("scope=?")
            params.append(scope)
        if model_id is not None:
            clauses.append("model_id=?")
            params.append(model_id)
        if model_revision is not None:
            clauses.append("model_revision=?")
            params.append(model_revision)
        where = f"WHERE {' AND '.join(clauses)}" if clauses else ""
        with self._lock:
            rows = self._conn.execute(
                f"SELECT status, COUNT(*) FROM templates {where} GROUP BY status",
                params,
            ).fetchall()
            total_events = self._conn.execute(
                f"SELECT COALESCE(SUM(occurrence_count),0) FROM templates {where}",
                params,
            ).fetchone()[0]
        by_status = {r[0]: r[1] for r in rows}
        return {
            "template_cardinality": sum(by_status.values()),
            "templates_ready": by_status.get("ready", 0),
            "templates_pending": by_status.get("pending", 0),
            "templates_failed": by_status.get("failed", 0),
            "template_occurrences": total_events,
        }


# ── Resolver ──────────────────────────────────────────────────────────────────

# Per-text lookup states returned by the registry-backed stages.
STATE_READY = "ready"
STATE_NEW = "new"
STATE_PENDING = "pending"
STATE_BACKOFF = "backoff"


def _validate_vector(vec: Any, dim: int) -> bool:
    """Provider-output validation: shape, finite values, non-zero norm."""
    try:
        arr = np.asarray(vec, dtype=np.float32)
    except Exception:
        return False
    if arr.ndim != 1 or arr.shape[0] != dim or dim <= 0:
        return False
    if not np.isfinite(arr).all():
        return False
    return float(np.linalg.norm(arr)) > 0.0


def _vector_from_bytes(blob: bytes, dim: int) -> np.ndarray:
    arr = np.frombuffer(blob, dtype=np.float32)
    if arr.shape[0] != dim:
        raise ValueError("stored vector dim mismatch")
    return arr.copy()


class TemplateDedup:
    """Three-stage dedup resolver binding normalizer, registry and embedder.

    Args:
        registry: persistent :class:`TemplateRegistry`.
        embedder: object with ``embed_batch(texts) -> ndarray``,
            ``model_name``, ``model_revision`` and ``dim`` attributes
            (i.e. :class:`bonsai_ml.text_embeddings.TextEmbedder`).
        scope: data-sharing/authorization scope for cache keys.
        model_id / model_revision: model identity for cache keys.
        norm_version: normalization contract version for cache keys.
        lru: optional LRU cache (defaults to a bounded internal cache; an
            ``MlMemoryManager.embedding_cache`` may be supplied).
        claim_ttl_ns: lease for in-flight template embeddings.
        now_ns: injectable clock for tests.
    """

    def __init__(
        self,
        registry: TemplateRegistry,
        embedder: Any,
        scope: str = DEFAULT_SCOPE,
        model_id: Optional[str] = None,
        model_revision: str = "",
        norm_version: str = NORMALIZATION_VERSION,
        lru: Optional[Any] = None,
        claim_ttl_ns: Optional[int] = None,
        now_ns: Optional[Callable[[], int]] = None,
    ) -> None:
        self.registry = registry
        self.embedder = embedder
        self.scope = scope
        self.model_id = model_id or str(getattr(embedder, "model_name", "unknown"))
        self.model_revision = (
            model_revision
            if model_revision
            else str(getattr(embedder, "model_revision", "") or "")
        )
        self.norm_version = norm_version
        self.normalizer = MessageNormalizer()
        self.lru = lru if lru is not None else _LruCache()
        if claim_ttl_ns is not None:
            self.registry.claim_ttl_ns = claim_ttl_ns
        self._now_ns = now_ns or time.time_ns
        self._claim_lock = threading.Lock()
        self.owner = f"{os.getpid()}:{uuid.uuid4().hex[:8]}"
        self.metrics = DedupMetrics()

    # ── public API ─────────────────────────────────────────────────────────

    def resolve_batch(self, texts: list[str]) -> list[Optional[np.ndarray]]:
        """Resolve embeddings for raw message texts.

        Returns a list aligned with ``texts``: a float32 vector on success,
        ``None`` when the template is pending under another claim or in
        provider-failure backoff (the event must remain unembedded).
        """
        m = DedupMetrics()
        m.events_seen = len(texts)
        n = len(texts)
        results: list[Optional[np.ndarray]] = [None] * n
        resolved_keys: list[Optional[str]] = [None] * n
        map_writes: list[tuple[int, str]] = []  # event idx -> template fp
        if not texts:
            self._accumulate(m)
            return results

        # Stage 1: exact fingerprint ----------------------------------------
        unresolved: list[int] = []
        template_of: dict[int, tuple[str, str]] = {}
        for i, text in enumerate(texts):
            if not text or not text.strip():
                continue
            hit = self._lookup_exact(text, m)
            if hit is not None:
                vec, ck = hit
                results[i] = vec
                resolved_keys[i] = ck
            else:
                tpl_text, tpl_fp = self.normalizer.template_fingerprint(text)
                template_of[i] = (tpl_text, tpl_fp)
                unresolved.append(i)

        # Stage 2: normalized template fingerprint ---------------------------
        still: list[int] = []
        for i in unresolved:
            tpl_text, tpl_fp = template_of[i]
            vec, state, ck = self._lookup_template(tpl_text, tpl_fp, m)
            if vec is not None:
                results[i] = vec
                resolved_keys[i] = ck
                map_writes.append((i, tpl_fp))
            elif state == STATE_PENDING:
                m.pending_skips += 1
            elif state == STATE_BACKOFF:
                m.backoff_skips += 1
            else:
                still.append(i)

        # Stage 3: claim and embed unique new templates ----------------------
        if still:
            self._embed_new_templates(
                texts, template_of, still, results, resolved_keys, map_writes, m
            )

        m.events_resolved = sum(1 for v in results if v is not None)

        # Bookkeeping: exact->template mappings and occurrence counts. --------
        try:
            now = self._now_ns()
            mappings = []
            bumps: list[str] = []
            mw = dict(map_writes)
            for i, ck in enumerate(resolved_keys):
                if ck is None:
                    continue
                bumps.append(ck)
                tpl_fp = mw.get(i)
                if tpl_fp is not None:
                    mappings.append(
                        (
                            self.scope, self.model_id, self.model_revision,
                            self.norm_version,
                            self.normalizer.fingerprint(texts[i]),
                            ck, now,
                        )
                    )
            self.registry.store_exact_mappings(mappings)
            self.registry.bump_occurrences(bumps, now)
        except Exception as exc:
            log.debug("dedup bookkeeping failed: %s", exc)

        self._accumulate(m)
        return results

    def stats_snapshot(self) -> dict[str, Any]:
        """Cumulative metrics plus registry cardinality for this scope/model."""
        snap = self.metrics.snapshot()
        snap.update(
            self.registry.cardinality(
                scope=self.scope,
                model_id=self.model_id,
                model_revision=self.model_revision,
            )
        )
        return snap

    # ── internals ──────────────────────────────────────────────────────────

    def _lru_key(self, kind: str, fp: str) -> str:
        raw = "|".join(
            (self.scope, self.model_id, self.model_revision,
             self.norm_version, kind, fp)
        ).encode()
        return hashlib.sha256(raw).hexdigest()

    def _lookup_exact(
        self, text: str, m: DedupMetrics
    ) -> Optional[tuple[np.ndarray, str]]:
        fp = self.normalizer.fingerprint(text)
        key = self._lru_key("exact", fp)
        hit = self.lru.get(key)
        if hit is not None:
            m.exact_hits += 1
            return hit[1], hit[0]
        cache_key = self.registry.exact_lookup(
            self.scope, self.model_id, self.model_revision,
            self.norm_version, fp,
        )
        if cache_key is None:
            return None
        row = self.registry.get_template(cache_key)
        if not row or row["status"] != "ready" or row["vector"] is None:
            return None
        try:
            vec = _vector_from_bytes(row["vector"], row["dim"])
        except Exception:
            return None
        self.lru.put(key, (cache_key, vec))
        m.exact_hits += 1
        return vec, cache_key

    def _lookup_template(
        self, tpl_text: str, tpl_fp: str, m: DedupMetrics
    ) -> tuple[Optional[np.ndarray], str, Optional[str]]:
        """Stage-2 lookup. Returns (vector, state, cache_key)."""
        key = self._lru_key("template", tpl_fp)
        hit = self.lru.get(key)
        if hit is not None:
            m.template_hits += 1
            return hit[1], STATE_READY, hit[0]

        state, row = self.registry.template_state(
            self.scope, self.model_id, self.model_revision,
            self.norm_version, tpl_fp,
        )
        cache_key = _cache_key(
            self.scope, self.model_id, self.model_revision,
            self.norm_version, tpl_fp,
        )
        if state == STATE_READY and row and row.get("vector"):
            try:
                vec = _vector_from_bytes(row["vector"], row["dim"])
            except Exception:
                return None, STATE_BACKOFF, cache_key
            self.lru.put(key, (cache_key, vec))
            m.template_hits += 1
            return vec, STATE_READY, cache_key
        return None, state, cache_key

    def _embed_new_templates(
        self,
        texts: list[str],
        template_of: dict[int, tuple[str, str]],
        still: list[int],
        results: list[Optional[np.ndarray]],
        resolved_keys: list[Optional[str]],
        map_writes: list[tuple[int, str]],
        m: DedupMetrics,
    ) -> None:
        # Group unresolved events by template fingerprint.
        groups: dict[str, dict[str, Any]] = {}
        for i in still:
            tpl_text, tpl_fp = template_of[i]
            g = groups.setdefault(
                tpl_fp, {"text": tpl_text, "members": [], "cache_key": None}
            )
            g["members"].append(i)
            g["cache_key"] = _cache_key(
                self.scope, self.model_id, self.model_revision,
                self.norm_version, tpl_fp,
            )

        to_embed: list[tuple[str, str, str]] = []  # (tpl_fp, tpl_text, cache_key)
        with self._claim_lock:
            for tpl_fp, g in groups.items():
                state, payload = self.registry.claim_or_get(
                    scope=self.scope,
                    model_id=self.model_id,
                    model_revision=self.model_revision,
                    norm_version=self.norm_version,
                    fingerprint=tpl_fp,
                    template_text=g["text"],
                    owner=self.owner,
                )
                if state == "claimed":
                    to_embed.append((tpl_fp, g["text"], g["cache_key"]))
                    m.new_templates_claimed += 1
                elif state == "ready" and payload and payload.get("vector"):
                    try:
                        vec = _vector_from_bytes(payload["vector"], payload["dim"])
                    except Exception:
                        vec = None
                    if vec is not None:
                        for i in g["members"]:
                            results[i] = vec
                            resolved_keys[i] = g["cache_key"]
                            map_writes.append((i, tpl_fp))
                            m.template_hits += 1
                        self.lru.put(
                            self._lru_key("template", tpl_fp),
                            (g["cache_key"], vec),
                        )
                elif state == "pending":
                    m.pending_skips += len(g["members"])
                else:
                    m.backoff_skips += len(g["members"])

            if not to_embed:
                return

            fps = [t[0] for t in to_embed]
            tpl_texts = [t[1] for t in to_embed]
            cache_keys = [t[2] for t in to_embed]

            t0 = time.monotonic()
            try:
                vectors = self.embedder.embed_batch(tpl_texts)
            except Exception as exc:
                latency = (time.monotonic() - t0) * 1000.0
                m.provider_calls += 1
                m.provider_failures += len(cache_keys)
                m.embed_latency_ms_total += latency
                m.embed_texts_total += len(tpl_texts)
                for ck in cache_keys:
                    self.registry.fail_template(ck, f"provider_error: {exc}")
                log.warning(
                    "TemplateDedup: provider failed for %d templates: %s",
                    len(cache_keys), exc,
                )
                return

            latency = (time.monotonic() - t0) * 1000.0
            m.provider_calls += 1
            m.embed_latency_ms_total += latency
            m.embed_texts_total += len(tpl_texts)
            if len(vectors) != len(tpl_texts):
                m.provider_failures += len(cache_keys)
                for ck in cache_keys:
                    self.registry.fail_template(
                        ck, "provider_error: embedding count mismatch"
                    )
                log.warning(
                    "TemplateDedup: provider returned %d vectors for %d texts",
                    len(vectors), len(tpl_texts),
                )
                return
            m.provider_texts_embedded += len(tpl_texts)

            dim = int(getattr(self.embedder, "dim", 0) or 0)
            ok_vecs: dict[str, np.ndarray] = {}
            for fp, ck, vec in zip(fps, cache_keys, vectors):
                if _validate_vector(vec, dim):
                    arr = np.asarray(vec, dtype=np.float32)
                    self.registry.complete_template(ck, arr, dim)
                    ok_vecs[fp] = arr
                    m.templates_embedded += 1
                else:
                    m.invalid_vectors += 1
                    self.registry.fail_template(
                        ck, "invalid_vector: dim/nonfinite/zero-norm"
                    )

            fp_to_ck = {fp: ck for fp, ck in zip(fps, cache_keys)}
            for tpl_fp, g in groups.items():
                vec = ok_vecs.get(tpl_fp)
                if vec is None:
                    continue
                ck = fp_to_ck[tpl_fp]
                self.lru.put(self._lru_key("template", tpl_fp), (ck, vec))
                for i in g["members"]:
                    results[i] = vec
                    resolved_keys[i] = ck
                    map_writes.append((i, tpl_fp))

    def _accumulate(self, m: DedupMetrics) -> None:
        agg = self.metrics
        agg.events_seen += m.events_seen
        agg.exact_hits += m.exact_hits
        agg.template_hits += m.template_hits
        agg.new_templates_claimed += m.new_templates_claimed
        agg.templates_embedded += m.templates_embedded
        agg.provider_texts_embedded += m.provider_texts_embedded
        agg.provider_calls += m.provider_calls
        agg.provider_failures += m.provider_failures
        agg.pending_skips += m.pending_skips
        agg.backoff_skips += m.backoff_skips
        agg.invalid_vectors += m.invalid_vectors
        agg.embed_latency_ms_total += m.embed_latency_ms_total
        agg.embed_texts_total += m.embed_texts_total
        agg.events_posted += m.events_posted
        agg.events_resolved += m.events_resolved


# ── construction helpers ──────────────────────────────────────────────────────


def registry_path_from_config(config: dict[str, Any]) -> str:
    return str(
        config.get("template_registry_path")
        or os.environ.get("BONSAI_TEMPLATE_REGISTRY_PATH")
        or DEFAULT_REGISTRY_PATH
    )


def dedup_scope_from_config(config: dict[str, Any]) -> str:
    return str(
        config.get("dedup_scope")
        or os.environ.get("BONSAI_EMBEDDING_SCOPE")
        or DEFAULT_SCOPE
    )


def build_template_dedup(
    config: dict[str, Any], embedder: Any
) -> Optional[TemplateDedup]:
    """Build a TemplateDedup from embedding config, or None when disabled."""
    if config.get("dedup_enabled", True) is False:
        return None
    registry = TemplateRegistry(
        path=registry_path_from_config(config),
        claim_ttl_ns=int(config.get("dedup_claim_ttl_secs", DEFAULT_CLAIM_TTL_SECS))
        * 1_000_000_000,
    )
    return TemplateDedup(
        registry=registry,
        embedder=embedder,
        scope=dedup_scope_from_config(config),
        model_id=str(getattr(embedder, "model_name", "unknown")),
        model_revision=str(getattr(embedder, "model_revision", "") or ""),
        lru=_LruCache(
            max_size=int(config.get("dedup_lru_entries", DEFAULT_LRU_ENTRIES))
        ),
    )
