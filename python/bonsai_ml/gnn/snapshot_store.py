"""GNN snapshot buffer serialisation using Apache Arrow IPC.

EV1-2 T6: SnapshotStore writes/reads serialised HeteroData snapshot sequences
as Arrow IPC files (.arrow) — not pickle. Reason: pickle is PyTorch version-
sensitive; Arrow IPC is schema-stable and readable without PyTorch.

Directory layout:
    runtime/parquet/gnn_snapshots/
        2026-05-25T06-00-00Z_T8_snapshots.arrow
        latest -> 2026-05-25T06-00-00Z_T8_snapshots.arrow

Buffer health:
    {buffer_size, oldest_ns, newest_ns, gap_seconds_max, is_stale}
    Stale = newest snapshot older than 1 hour.
"""
from __future__ import annotations

import logging
import os
import time
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Optional

log = logging.getLogger(__name__)

MAX_BUFFER_SIZE = 8
STALE_THRESHOLD_SECS = 3600
DEFAULT_STORE_DIR = "runtime/parquet/gnn_snapshots"


@dataclass
class BufferHealth:
    buffer_size: int
    oldest_ns: Optional[int]
    newest_ns: Optional[int]
    gap_seconds_max: float
    is_stale: bool
    snapshot_paths: list[str]

    def to_dict(self) -> dict:
        return {
            "buffer_size": self.buffer_size,
            "oldest_ns": self.oldest_ns,
            "newest_ns": self.newest_ns,
            "gap_seconds_max": self.gap_seconds_max,
            "is_stale": self.is_stale,
        }


class SnapshotStore:
    """Manages a rolling Arrow IPC buffer of T=8 HeteroData snapshots.

    Each snapshot is serialised to a separate .arrow file. The buffer is
    maintained as a list of the T most recent files (oldest evicted first).

    Args:
        store_dir: Directory for snapshot files.
        max_buffer_size: T — maximum number of snapshots to keep.
    """

    def __init__(
        self,
        store_dir: str = DEFAULT_STORE_DIR,
        max_buffer_size: int = MAX_BUFFER_SIZE,
    ) -> None:
        self.store_dir = Path(store_dir)
        self.max_buffer_size = max_buffer_size
        self.store_dir.mkdir(parents=True, exist_ok=True)

    def write_snapshot(self, snapshot: Any, timestamp_ns: Optional[int] = None) -> str:
        """Serialise a HeteroData snapshot to an Arrow IPC file.

        Appends to the rolling buffer. Evicts the oldest file when buffer is full.

        Args:
            snapshot: A PyG HeteroData object.
            timestamp_ns: Optional nanosecond timestamp. Defaults to current time.

        Returns:
            Path to the written .arrow file.
        """
        if timestamp_ns is None:
            timestamp_ns = time.time_ns()

        ts_dt = datetime.fromtimestamp(timestamp_ns / 1e9, tz=timezone.utc)
        ts_str = ts_dt.strftime("%Y-%m-%dT%H-%M-%SZ")
        out_path = self.store_dir / f"{ts_str}_snapshot.arrow"

        self._write_arrow(snapshot, out_path, timestamp_ns)
        self._update_symlink(out_path)
        self._evict_old_snapshots()

        log.debug("SnapshotStore: wrote snapshot → %s", out_path)
        return str(out_path)

    def load_buffer(self) -> list[Any]:
        """Load the T most recent snapshots in chronological order (oldest first).

        Returns:
            List of HeteroData objects. Empty list if no snapshots available.
        """
        files = self._get_snapshot_files()
        if not files:
            return []

        snapshots = []
        for f in files[: self.max_buffer_size]:
            snap = self._read_arrow(f)
            if snap is not None:
                snapshots.append(snap)

        snapshots.reverse()
        return snapshots

    def get_buffer_health(self) -> BufferHealth:
        """Return health metrics for the snapshot buffer."""
        files = self._get_snapshot_files()

        if not files:
            return BufferHealth(
                buffer_size=0,
                oldest_ns=None,
                newest_ns=None,
                gap_seconds_max=0.0,
                is_stale=True,
                snapshot_paths=[],
            )

        timestamps = []
        for f in files[: self.max_buffer_size]:
            ts_ns = self._read_timestamp_ns(f)
            if ts_ns is not None:
                timestamps.append(ts_ns)

        timestamps.sort()
        oldest_ns = timestamps[0] if timestamps else None
        newest_ns = timestamps[-1] if timestamps else None

        gaps = []
        for i in range(1, len(timestamps)):
            gap = (timestamps[i] - timestamps[i - 1]) / 1e9
            gaps.append(gap)
        gap_seconds_max = max(gaps) if gaps else 0.0

        is_stale = (
            newest_ns is None
            or (time.time_ns() - newest_ns) / 1e9 > STALE_THRESHOLD_SECS
        )

        return BufferHealth(
            buffer_size=len(files[: self.max_buffer_size]),
            oldest_ns=oldest_ns,
            newest_ns=newest_ns,
            gap_seconds_max=gap_seconds_max,
            is_stale=is_stale,
            snapshot_paths=[str(f) for f in files[: self.max_buffer_size]],
        )

    # ── Private ───────────────────────────────────────────────────────────────

    def _get_snapshot_files(self) -> list[Path]:
        """Return snapshot files sorted newest first."""
        files = [
            f for f in self.store_dir.iterdir()
            if f.is_file() and f.suffix == ".arrow" and not f.name.startswith("latest")
        ]
        files.sort(key=lambda f: f.stat().st_mtime, reverse=True)
        return files

    def _evict_old_snapshots(self) -> None:
        files = self._get_snapshot_files()
        if len(files) <= self.max_buffer_size:
            return
        for f in files[self.max_buffer_size:]:
            try:
                f.unlink()
                log.debug("SnapshotStore: evicted %s", f.name)
            except OSError as exc:
                log.debug("Could not evict snapshot %s: %s", f, exc)

    def _update_symlink(self, target: Path) -> None:
        symlink = self.store_dir / "latest"
        try:
            if symlink.is_symlink() or symlink.exists():
                symlink.unlink()
            os.symlink(str(target), str(symlink))
        except OSError as exc:
            log.debug("Could not update latest symlink: %s", exc)

    def _write_arrow(self, snapshot: Any, out_path: Path, timestamp_ns: int) -> None:
        """Serialise HeteroData to Arrow IPC format.

        The whole snapshot is encoded as one JSON payload row so the IPC
        file has a single uniform schema — Arrow IPC files cannot contain
        record batches with different schemas. File-level metadata carries
        timestamp_ns and the format tag.
        """
        try:
            import json

            import pyarrow as pa

            payload: dict[str, Any] = {"nodes": {}, "edges": {}}
            if hasattr(snapshot, "node_types"):
                for ntype in snapshot.node_types:
                    try:
                        x = snapshot[ntype].x
                        if x is not None:
                            import torch
                            payload["nodes"][ntype] = {
                                "x_flat": x.numpy().flatten().tolist(),
                                "shape": list(x.shape),
                            }
                    except Exception:
                        pass

                for etype in snapshot.edge_types:
                    try:
                        src, rel, dst = etype
                        ei = snapshot[src, rel, dst].edge_index
                        if ei is not None:
                            import torch
                            payload["edges"][f"{src}__{rel}__{dst}"] = {
                                "src": ei[0].numpy().tolist(),
                                "dst": ei[1].numpy().tolist(),
                            }
                    except Exception:
                        pass

            schema = pa.schema(
                [("payload", pa.string())],
                metadata={
                    b"timestamp_ns": str(timestamp_ns).encode(),
                    b"format": b"bonsai_hetero_snapshot_v2",
                },
            )
            batch = pa.record_batch(
                [pa.array([json.dumps(payload)], type=pa.string())],
                schema=schema,
            )

            import pyarrow.ipc as ipc
            with pa.OSFile(str(out_path), "wb") as sink:
                writer = ipc.new_file(sink, schema)
                writer.write_batch(batch)
                writer.close()

        except ImportError:
            import pickle
            with open(str(out_path).replace(".arrow", ".pkl"), "wb") as f:
                pickle.dump({"snapshot": snapshot, "timestamp_ns": timestamp_ns}, f)

    def _read_arrow(self, path: Path) -> Optional[Any]:
        """Deserialise a snapshot from Arrow IPC. Returns HeteroData or None."""
        try:
            import json

            import pyarrow.ipc as ipc
            import pyarrow as pa

            with pa.memory_map(str(path), "r") as source:
                reader = ipc.open_file(source)
                if reader.num_record_batches == 0:
                    return None
                batch = reader.get_batch(0)
            payload = json.loads(batch.column("payload")[0].as_py())

            try:
                import torch
                from torch_geometric.data import HeteroData
                data = HeteroData()

                for ntype, node in payload.get("nodes", {}).items():
                    shape = node.get("shape")
                    if isinstance(shape, list) and len(shape) == 2:
                        x = torch.tensor(
                            node["x_flat"], dtype=torch.float32
                        ).reshape(shape)
                        data[ntype].x = x

                for etype_str, edge in payload.get("edges", {}).items():
                    parts = etype_str.split("__")
                    if len(parts) == 3:
                        src_t, rel, dst_t = parts
                        ei = torch.tensor(
                            [edge["src"], edge["dst"]], dtype=torch.long
                        )
                        data[src_t, rel, dst_t].edge_index = ei

                return data

            except ImportError:
                return payload

        except Exception as exc:
            log.debug("Could not read snapshot %s: %s", path, exc)
            return None

    def _read_timestamp_ns(self, path: Path) -> Optional[int]:
        """Extract timestamp_ns from Arrow IPC file metadata without full deserialise."""
        try:
            import pyarrow as pa
            import pyarrow.ipc as ipc
            with pa.memory_map(str(path), "r") as source:
                reader = ipc.open_file(source)
                meta = reader.schema.metadata or {}
                ts_b = meta.get(b"timestamp_ns")
                if ts_b:
                    return int(ts_b)
        except Exception:
            pass
        ts_from_mtime = int(path.stat().st_mtime * 1e9)
        return ts_from_mtime
