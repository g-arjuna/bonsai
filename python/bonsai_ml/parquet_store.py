"""Parquet archive directory manager for Bonsai ML pipeline.

EV1-2 T5: Manages runtime/parquet/ layout with per-type subdirectories,
'latest' symlinks, and cleanup of old files.

Directory layout:
    runtime/parquet/
      anomaly/
        2026-05-25T02-00-00Z_v1_8542rows.parquet
        latest -> 2026-05-25T02-00-00Z_v1_8542rows.parquet
      remediation/
        2026-05-19T02-00-00Z_v1_1203rows.parquet
        latest -> ...
      gnn_snapshots/
        2026-05-25T06-00-00Z_T8_snapshots.pkl
        latest -> ...
"""
from __future__ import annotations

import logging
import os
from dataclasses import dataclass
from pathlib import Path
from typing import Optional

log = logging.getLogger(__name__)

VALID_EXPORT_TYPES = ("anomaly", "remediation", "gnn_snapshots")
DEFAULT_KEEP_LAST_N = 10


@dataclass
class ExportFileInfo:
    """Metadata about a single file in the Parquet store."""
    path: str
    filename: str
    export_type: str
    size_bytes: int
    modified_ts: float
    row_count: Optional[int] = None

    def to_dict(self) -> dict:
        return {
            "path": self.path,
            "filename": self.filename,
            "export_type": self.export_type,
            "size_bytes": self.size_bytes,
            "modified_ts": self.modified_ts,
            "row_count": self.row_count,
        }


class ParquetStore:
    """Manages the parquet file archive under root_dir/parquet/.

    Args:
        root_dir: Root of the runtime directory. Parquet files are stored
            under root_dir/parquet/{export_type}/.
    """

    def __init__(self, root_dir: str = "runtime") -> None:
        self.root = Path(root_dir) / "parquet"

    def _type_dir(self, export_type: str) -> Path:
        d = self.root / export_type
        d.mkdir(parents=True, exist_ok=True)
        return d

    def register(self, export_type: str, path: str, rows: int = 0) -> None:
        """Register a new export file and update the 'latest' symlink.

        Args:
            export_type: "anomaly", "remediation", or "gnn_snapshots".
            path: Absolute or relative path to the .parquet file.
            rows: Row count, used for renaming if the file path does not
                already contain row count metadata.
        """
        src = Path(path).resolve()
        type_dir = self._type_dir(export_type)

        target_path = src
        if not str(src).startswith(str(type_dir)):
            target_path = type_dir / src.name
            if not target_path.exists():
                try:
                    import shutil
                    shutil.copy2(str(src), str(target_path))
                    log.debug("Copied %s → %s", src, target_path)
                except Exception as exc:
                    log.warning("Could not copy to store: %s", exc)
                    target_path = src

        self._update_symlink(export_type, target_path)
        log.info(
            "ParquetStore: registered %s/%s (%d rows)",
            export_type, target_path.name, rows,
        )

    def get_latest(self, export_type: str) -> Optional[str]:
        """Return the path of the latest export for the given type, or None."""
        type_dir = self._type_dir(export_type)
        symlink = type_dir / "latest"
        if symlink.is_symlink():
            target = symlink.resolve()
            if target.exists():
                return str(target)
        files = sorted(
            [f for f in type_dir.glob("*.parquet") if not f.name == "latest"],
            key=lambda f: f.stat().st_mtime,
        )
        if files:
            return str(files[-1])
        return None

    def list_exports(self, export_type: str) -> list[ExportFileInfo]:
        """Return all export files for a type, sorted newest first."""
        type_dir = self._type_dir(export_type)
        results: list[ExportFileInfo] = []

        for f in type_dir.iterdir():
            if f.is_symlink() or not f.is_file():
                continue
            suffix = f.suffix.lower()
            if suffix not in (".parquet", ".pkl", ".arrow"):
                continue
            stat = f.stat()
            rows = _parse_rows_from_filename(f.name)
            results.append(ExportFileInfo(
                path=str(f),
                filename=f.name,
                export_type=export_type,
                size_bytes=stat.st_size,
                modified_ts=stat.st_mtime,
                row_count=rows,
            ))

        results.sort(key=lambda fi: fi.modified_ts, reverse=True)
        return results

    def cleanup_old(self, export_type: str, keep_last_n: int = DEFAULT_KEEP_LAST_N) -> int:
        """Remove all but the most recent keep_last_n files.

        Preserves the 'latest' symlink and does not remove the file it points to.

        Returns:
            Number of files removed.
        """
        all_files = self.list_exports(export_type)

        if len(all_files) <= keep_last_n:
            return 0

        type_dir = self._type_dir(export_type)
        latest_link = type_dir / "latest"
        protected: set[str] = set()
        if latest_link.is_symlink():
            try:
                protected.add(str(latest_link.resolve()))
            except OSError:
                pass

        to_remove = all_files[keep_last_n:]
        removed = 0
        for fi in to_remove:
            if fi.path in protected:
                continue
            try:
                os.remove(fi.path)
                removed += 1
                log.debug("Removed old export: %s", fi.path)
            except OSError as exc:
                log.warning("Could not remove %s: %s", fi.path, exc)

        log.info(
            "ParquetStore cleanup: removed %d old %s files (kept %d)",
            removed, export_type, keep_last_n,
        )
        return removed

    def get_store_summary(self) -> dict[str, dict]:
        """Return a summary dict of all export types."""
        summary: dict[str, dict] = {}
        for export_type in VALID_EXPORT_TYPES:
            files = self.list_exports(export_type)
            latest = self.get_latest(export_type)
            total_bytes = sum(fi.size_bytes for fi in files)
            summary[export_type] = {
                "file_count": len(files),
                "total_size_bytes": total_bytes,
                "latest_path": latest,
                "latest_rows": files[0].row_count if files else None,
                "oldest_modified_ts": files[-1].modified_ts if files else None,
                "newest_modified_ts": files[0].modified_ts if files else None,
            }
        return summary

    def _update_symlink(self, export_type: str, target: Path) -> None:
        type_dir = self._type_dir(export_type)
        symlink = type_dir / "latest"
        try:
            if symlink.is_symlink() or symlink.exists():
                symlink.unlink()
            os.symlink(str(target), str(symlink))
        except OSError as exc:
            log.debug("Could not update 'latest' symlink for %s: %s", export_type, exc)


def _parse_rows_from_filename(filename: str) -> Optional[int]:
    """Extract row count from a filename like 2026-05-25T02-00-00Z_v1_8542rows.parquet."""
    import re
    match = re.search(r"_(\d+)rows", filename)
    if match:
        return int(match.group(1))
    return None
