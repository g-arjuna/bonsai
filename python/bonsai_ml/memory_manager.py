"""ML memory manager for Bonsai sidecar.

EV1-9 T5: MlMemoryManager tracks per-component memory usage and enforces
LRU eviction to prevent OOM kills on constrained hardware.

Components tracked:
  - ModelCache: loaded STGNN + IsolationForest models
  - EmbeddingCache: in-memory embedding vectors (LRU, max 10,000 entries)
  - SnapshotBuffer: 8 snapshots × N_devices × 40 dims × 4 bytes ≈ 50KB/100 devices

Periodic check (every 5 min): if total RSS > max_total_memory_mb, evict.
"""
from __future__ import annotations

import logging
import os
import threading
import time
from collections import OrderedDict
from typing import Any, Optional

log = logging.getLogger(__name__)

DEFAULT_MAX_TOTAL_MB = int(os.environ.get("BONSAI_SIDECAR_MAX_MEM_MB", "1800"))
DEFAULT_MAX_MODEL_MB = 1024
DEFAULT_MAX_EMBEDDING_ENTRIES = 10_000
CHECK_INTERVAL_SECS = 300


class _LruCache:
    """Thread-safe LRU cache with a fixed maximum size."""

    def __init__(self, max_size: int = DEFAULT_MAX_EMBEDDING_ENTRIES) -> None:
        self.max_size = max_size
        self._cache: OrderedDict = OrderedDict()
        self._lock = threading.Lock()
        self.hits = 0
        self.misses = 0
        self.evictions = 0

    def get(self, key: str) -> Optional[Any]:
        with self._lock:
            if key not in self._cache:
                self.misses += 1
                return None
            self._cache.move_to_end(key)
            self.hits += 1
            return self._cache[key]

    def put(self, key: str, value: Any) -> None:
        with self._lock:
            if key in self._cache:
                self._cache.move_to_end(key)
                self._cache[key] = value
            else:
                if len(self._cache) >= self.max_size:
                    self._cache.popitem(last=False)
                    self.evictions += 1
                self._cache[key] = value

    def clear(self) -> int:
        with self._lock:
            n = len(self._cache)
            self._cache.clear()
            return n

    def __len__(self) -> int:
        with self._lock:
            return len(self._cache)

    def stats(self) -> dict:
        with self._lock:
            return {
                "size": len(self._cache),
                "max_size": self.max_size,
                "hits": self.hits,
                "misses": self.misses,
                "evictions": self.evictions,
            }


class MlMemoryManager:
    """Per-component memory tracker + periodic eviction.

    Args:
        max_total_memory_mb: RSS threshold for emergency eviction.
        max_model_memory_mb: Model cache size limit.
        max_embedding_entries: Max LRU embedding cache entries.
    """

    def __init__(
        self,
        max_total_memory_mb: int = DEFAULT_MAX_TOTAL_MB,
        max_model_memory_mb: int = DEFAULT_MAX_MODEL_MB,
        max_embedding_entries: int = DEFAULT_MAX_EMBEDDING_ENTRIES,
    ) -> None:
        self.max_total_memory_mb = max_total_memory_mb
        self.max_model_memory_mb = max_model_memory_mb

        self.embedding_cache = _LruCache(max_size=max_embedding_entries)
        self._loaded_models: dict[str, Any] = {}
        self._model_sizes_mb: dict[str, float] = {}
        self._lock = threading.Lock()

        self._check_thread = threading.Thread(
            target=self._check_loop, daemon=True, name="ml-memory-manager"
        )
        self._check_thread.start()

    def register_model(self, model_id: str, model: Any, size_mb: float = 0.0) -> None:
        """Register a loaded model in the cache."""
        with self._lock:
            self._loaded_models[model_id] = model
            if size_mb == 0.0:
                size_mb = self._estimate_model_size_mb(model)
            self._model_sizes_mb[model_id] = size_mb
            log.debug("MemoryManager: registered model %s (%.1fMB)", model_id, size_mb)
            self._evict_models_if_needed()

    def unregister_model(self, model_id: str) -> None:
        with self._lock:
            self._loaded_models.pop(model_id, None)
            self._model_sizes_mb.pop(model_id, None)

    def get_model(self, model_id: str) -> Optional[Any]:
        with self._lock:
            return self._loaded_models.get(model_id)

    def get_memory_report(self) -> dict:
        """Return per-component memory estimates for health endpoint."""
        rss_mb = self._get_rss_mb()
        with self._lock:
            model_total = sum(self._model_sizes_mb.values())
            model_breakdown = dict(self._model_sizes_mb)

        emb_size = len(self.embedding_cache)
        emb_est_mb = emb_size * 1536 * 4 / 1024 / 1024

        return {
            "rss_mb": rss_mb,
            "model_cache_mb": model_total,
            "model_breakdown": model_breakdown,
            "embedding_cache_entries": emb_size,
            "embedding_cache_est_mb": round(emb_est_mb, 2),
            "embedding_cache_stats": self.embedding_cache.stats(),
            "max_total_mb": self.max_total_memory_mb,
            "pressure": rss_mb > self.max_total_memory_mb * 0.85,
        }

    # ── Private ───────────────────────────────────────────────────────────────

    def _check_loop(self) -> None:
        while True:
            time.sleep(CHECK_INTERVAL_SECS)
            try:
                self._run_check()
            except Exception as exc:
                log.debug("MemoryManager check error: %s", exc)

    def _run_check(self) -> None:
        rss_mb = self._get_rss_mb()
        if rss_mb > self.max_total_memory_mb:
            log.warning(
                "MemoryManager: RSS %.0fMB > limit %.0fMB — evicting caches",
                rss_mb, self.max_total_memory_mb,
            )
            n = self.embedding_cache.clear()
            log.warning("MemoryManager: cleared %d embedding cache entries", n)
            with self._lock:
                self._evict_models_if_needed()

        self._emit_metrics(rss_mb)

    def _evict_models_if_needed(self) -> None:
        """Evict LRU model if total model memory exceeds limit. Caller holds _lock."""
        while sum(self._model_sizes_mb.values()) > self.max_model_memory_mb:
            if not self._model_sizes_mb:
                break
            oldest = next(iter(self._model_sizes_mb))
            size = self._model_sizes_mb.pop(oldest)
            self._loaded_models.pop(oldest, None)
            log.info("MemoryManager: evicted model %s (%.1fMB)", oldest, size)

    def _get_rss_mb(self) -> float:
        try:
            import resource
            usage = resource.getrusage(resource.RUSAGE_SELF)
            rss = usage.ru_maxrss
            import sys
            if sys.platform == "darwin":
                rss = rss / 1024 / 1024
            else:
                rss = rss / 1024
            return rss
        except Exception:
            pass
        try:
            with open("/proc/self/status") as f:
                for line in f:
                    if line.startswith("VmRSS"):
                        return int(line.split()[1]) / 1024
        except Exception:
            pass
        return 0.0

    def _estimate_model_size_mb(self, model: Any) -> float:
        try:
            import torch
            total = sum(p.numel() * p.element_size() for p in model.parameters())
            return total / 1024 / 1024
        except Exception:
            return 200.0

    def _emit_metrics(self, rss_mb: float) -> None:
        try:
            from prometheus_client import Gauge
            Gauge(
                "bonsai_sidecar_memory_mb",
                "Sidecar process RSS in MB",
            ).set(rss_mb)
        except Exception:
            pass
