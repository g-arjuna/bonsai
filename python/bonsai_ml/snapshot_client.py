"""Live graph snapshot client for Bonsai (EV1-4 T6).

Wraps GET /api/graph/snapshot so the STGNN inference loop can fetch a
structured live topology snapshot without embedding HTTP logic in the
inference code.

The snapshot payload contains:
  - devices: list of device dicts (address, vendor, role, site, scores, etc.)
  - links: list of LLDP/physical links
  - bgp_sessions: list of BGP session dicts
  - bfd_sessions: list of BFD session dicts
  - detections: recent DetectionEvents
  - snapshot_ns: Unix timestamp (nanoseconds) of the snapshot

Typical usage::

    client = GraphSnapshotClient(base_url="http://localhost:3000")
    snap = client.fetch()
    print(snap.snapshot_ns, len(snap.devices))
"""
from __future__ import annotations

import logging
import time
from dataclasses import dataclass, field
from typing import Any, Optional

log = logging.getLogger(__name__)


# ── Data classes ──────────────────────────────────────────────────────────────

@dataclass
class GraphSnapshot:
    """Structured live graph snapshot from Bonsai core."""

    snapshot_ns: int
    devices: list[dict] = field(default_factory=list)
    links: list[dict] = field(default_factory=list)
    bgp_sessions: list[dict] = field(default_factory=list)
    bfd_sessions: list[dict] = field(default_factory=list)
    isis_links: list[dict] = field(default_factory=list)
    detections: list[dict] = field(default_factory=list)

    @property
    def device_count(self) -> int:
        return len(self.devices)

    @property
    def anomalous_devices(self) -> list[dict]:
        """Devices whose most recent GnnScore was above threshold."""
        return [d for d in self.devices if d.get("gnn_anomalous", False)]

    def device_index(self) -> dict[str, dict]:
        """Return a dict keyed by device address for O(1) lookups."""
        return {d["address"]: d for d in self.devices}

    @classmethod
    def from_api_response(cls, data: dict) -> "GraphSnapshot":
        """Construct a GraphSnapshot from the raw /api/graph/snapshot JSON."""
        return cls(
            snapshot_ns=data.get("snapshot_ns", 0),
            devices=data.get("devices", []),
            links=data.get("links", []),
            bgp_sessions=data.get("bgp_sessions", []),
            bfd_sessions=data.get("bfd_sessions", []),
            isis_links=data.get("isis_links", []),
            detections=data.get("detections", []),
        )


# ── Client ────────────────────────────────────────────────────────────────────

class GraphSnapshotClient:
    """HTTP client for fetching live graph snapshots from Bonsai core.

    Args:
        base_url: Root URL of the Bonsai HTTP server (no trailing slash).
        timeout:  Per-request timeout in seconds.
        max_retries: Number of retries on transient connection errors.
        cache_ttl_secs: If > 0, cache the last snapshot for this many seconds
            to avoid hammering the server during rapid polling.
    """

    def __init__(
        self,
        base_url: str = "http://localhost:3000",
        timeout: float = 60.0,
        max_retries: int = 3,
        cache_ttl_secs: float = 0.0,
    ) -> None:
        self._base = base_url.rstrip("/")
        self._timeout = timeout
        self._max_retries = max_retries
        self._cache_ttl = cache_ttl_secs
        self._cached: Optional[GraphSnapshot] = None
        self._cached_at: float = 0.0
        self._session: Any = None

    # ── Internal helpers ──────────────────────────────────────────────────────

    def _session_get(self) -> Any:
        if self._session is None:
            try:
                import requests
                s = requests.Session()
                s.headers["Accept"] = "application/json"
                self._session = s
            except ImportError as exc:
                raise RuntimeError(
                    "requests library required: pip install requests"
                ) from exc
        return self._session

    def _http_get(self, path: str) -> dict:
        import requests

        url = f"{self._base}{path}"
        last_exc: Optional[Exception] = None
        for attempt in range(1, self._max_retries + 1):
            try:
                resp = self._session_get().get(url, timeout=self._timeout)
                resp.raise_for_status()
                return resp.json()
            except requests.exceptions.RequestException as exc:
                last_exc = exc
                if attempt < self._max_retries:
                    backoff = 2 ** (attempt - 1)
                    log.warning(
                        "GET %s attempt %d failed: %s — retrying in %ds",
                        path, attempt, exc, backoff,
                    )
                    time.sleep(backoff)
        raise RuntimeError(
            f"GET {path} failed after {self._max_retries} retries: {last_exc}"
        ) from last_exc

    # ── Public API ────────────────────────────────────────────────────────────

    def fetch(self, force: bool = False) -> GraphSnapshot:
        """Fetch (or return cached) live graph snapshot.

        Args:
            force: If True, bypass the cache even if TTL has not expired.

        Returns:
            GraphSnapshot populated from /api/graph/snapshot.

        Raises:
            RuntimeError: If the HTTP request fails after all retries.
        """
        now = time.monotonic()
        if (
            not force
            and self._cache_ttl > 0
            and self._cached is not None
            and (now - self._cached_at) < self._cache_ttl
        ):
            log.debug("returning cached graph snapshot (age=%.1fs)", now - self._cached_at)
            return self._cached

        data = self._http_get("/api/graph/snapshot")
        snap = GraphSnapshot.from_api_response(data)
        log.debug(
            "fetched graph snapshot: %d devices, %d links, snapshot_ns=%d",
            snap.device_count,
            len(snap.links),
            snap.snapshot_ns,
        )

        if self._cache_ttl > 0:
            self._cached = snap
            self._cached_at = now

        return snap

    def fetch_device_addresses(self) -> list[str]:
        """Convenience method: return only device addresses from snapshot."""
        try:
            snap = self.fetch()
            return [d["address"] for d in snap.devices if "address" in d]
        except Exception as exc:
            log.error("fetch_device_addresses failed: %s", exc)
            return []

    def close(self) -> None:
        """Close the underlying requests.Session."""
        if self._session is not None:
            try:
                self._session.close()
            except Exception:
                pass
            self._session = None

    def __enter__(self) -> "GraphSnapshotClient":
        return self

    def __exit__(self, *_: Any) -> None:
        self.close()
