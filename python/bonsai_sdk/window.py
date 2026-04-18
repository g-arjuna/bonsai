"""Sliding event window for multi-event pattern rules (e.g. flap counting)."""
from __future__ import annotations

import threading
import time
from collections import deque
from typing import Tuple


class EventWindow:
    """Thread-safe time-bounded deque of (timestamp_ns, event_type) entries.

    State is in-process only — resets on rule engine restart. Phase 5 will
    replace this with graph queries over the stored StateChangeEvent history.
    """

    def __init__(self, window_seconds: float = 300.0):
        self._window_ns = int(window_seconds * 1_000_000_000)
        self._entries: deque[Tuple[int, str]] = deque()
        self._lock = threading.Lock()

    def record(self, timestamp_ns: int, event_type: str) -> None:
        with self._lock:
            self._entries.append((timestamp_ns, event_type))
            self._prune(timestamp_ns)

    def count(self, event_type: str | None = None) -> int:
        """Count entries within the window, optionally filtered by event_type."""
        now_ns = time.time_ns()
        with self._lock:
            self._prune(now_ns)
            if event_type is None:
                return len(self._entries)
            return sum(1 for _, t in self._entries if t == event_type)

    def _prune(self, now_ns: int) -> None:
        cutoff = now_ns - self._window_ns
        while self._entries and self._entries[0][0] < cutoff:
            self._entries.popleft()


class WindowRegistry:
    """Per-device-peer sliding windows, created on demand."""

    def __init__(self, window_seconds: float = 300.0):
        self._windows: dict[str, EventWindow] = {}
        self._lock = threading.Lock()
        self._window_seconds = window_seconds

    def get(self, key: str) -> EventWindow:
        with self._lock:
            if key not in self._windows:
                self._windows[key] = EventWindow(self._window_seconds)
            return self._windows[key]
