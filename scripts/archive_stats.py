"""Summarize archived parquet batches under the Bonsai archive tree.

Requires project Python deps with pyarrow installed.
"""
from __future__ import annotations

import argparse
from collections import defaultdict
from pathlib import Path


def main() -> int:
    parser = argparse.ArgumentParser(description="Summarize Bonsai parquet archive files")
    parser.add_argument("archive_root", nargs="?", default="archive", help="Archive directory root")
    args = parser.parse_args()

    try:
        import pyarrow.parquet as pq
    except ImportError:
        print("pyarrow is required for archive_stats.py")
        return 1

    archive_root = Path(args.archive_root)
    files = sorted(archive_root.rglob("*.parquet"))
    if not files:
        print(f"No parquet files found under {archive_root}")
        return 0

    total_rows = 0
    total_bytes = 0
    total_uncompressed = 0
    total_compressed = 0
    per_device: dict[str, dict[str, int | None]] = defaultdict(
        lambda: {"rows": 0, "oldest": None, "newest": None}
    )

    for path in files:
        parquet_file = pq.ParquetFile(path)
        metadata = parquet_file.metadata
        total_rows += metadata.num_rows
        total_bytes += path.stat().st_size

        table = parquet_file.read(columns=["target", "timestamp_ns"])
        targets = table.column("target").to_pylist()
        timestamps = table.column("timestamp_ns").to_pylist()
        for target, ts in zip(targets, timestamps, strict=False):
            device_stats = per_device[target]
            device_stats["rows"] += 1
            device_stats["oldest"] = ts if device_stats["oldest"] is None else min(device_stats["oldest"], ts)
            device_stats["newest"] = ts if device_stats["newest"] is None else max(device_stats["newest"], ts)

        for row_group_idx in range(metadata.num_row_groups):
            row_group = metadata.row_group(row_group_idx)
            for column_idx in range(row_group.num_columns):
                column = row_group.column(column_idx)
                total_uncompressed += column.total_uncompressed_size
                total_compressed += column.total_compressed_size

    compression_ratio = (
        total_uncompressed / total_compressed if total_compressed else 0.0
    )

    print(f"archive_root: {archive_root}")
    print(f"files: {len(files)}")
    print(f"total_rows: {total_rows}")
    print(f"bytes_on_disk: {total_bytes}")
    print(f"compression_ratio: {compression_ratio:.2f}")
    print("per_device:")
    for target in sorted(per_device):
        stats = per_device[target]
        print(
            f"  - {target}: rows={stats['rows']} "
            f"oldest_timestamp_ns={stats['oldest']} newest_timestamp_ns={stats['newest']}"
        )

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
