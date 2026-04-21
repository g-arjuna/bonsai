#!/usr/bin/env python3
"""Query the graph and report whether Model A / Model C have enough data to train."""
from __future__ import annotations

import argparse
import json
import sys
import tomllib
import urllib.request
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parents[1] / "python"))

from bonsai_sdk.training_readiness import (
    build_graph_readiness_from_summary,
    format_check,
    query_graph_readiness,
)


def _load_local_endpoints() -> tuple[str, str]:
    config_path = Path(__file__).parents[1] / "bonsai.toml"
    api_addr = "[::1]:50051"
    http_url = "http://127.0.0.1:3000/api/readiness"

    if not config_path.exists():
        return api_addr, http_url

    data = tomllib.loads(config_path.read_text(encoding="utf-8"))
    return str(data.get("api_addr", api_addr)), http_url


def _query_via_grpc(api_addr: str):
    from bonsai_sdk.client import BonsaiClient

    with BonsaiClient(api_addr) as client:
        return query_graph_readiness(client)


def _query_via_http(http_url: str):
    with urllib.request.urlopen(http_url, timeout=10) as resp:
        summary = json.load(resp)
    return build_graph_readiness_from_summary(summary)


def main() -> None:
    default_api, default_http = _load_local_endpoints()
    ap = argparse.ArgumentParser()
    ap.add_argument("--api", default=default_api, help="Bonsai gRPC endpoint")
    ap.add_argument("--http", default=default_http, help="Bonsai HTTP readiness endpoint")
    ap.add_argument(
        "--transport",
        choices=["auto", "grpc", "http"],
        default="auto",
        help="How to query Bonsai readiness",
    )
    args = ap.parse_args()

    model_a = None
    model_c = None
    source = ""
    errors: list[str] = []

    if args.transport in {"auto", "grpc"}:
        try:
            model_a, model_c = _query_via_grpc(args.api)
            source = f"gRPC {args.api}"
        except Exception as exc:
            errors.append(f"gRPC {args.api}: {exc}")
            if args.transport == "grpc":
                print(f"ERROR: failed to query Bonsai via gRPC at {args.api}: {exc}", file=sys.stderr)
                raise SystemExit(1)

    if model_a is None and args.transport in {"auto", "http"}:
        try:
            model_a, model_c = _query_via_http(args.http)
            source = f"HTTP {args.http}"
        except Exception as exc:
            errors.append(f"HTTP {args.http}: {exc}")
            if args.transport == "http":
                print(f"ERROR: failed to query Bonsai via HTTP at {args.http}: {exc}", file=sys.stderr)
                raise SystemExit(1)

    if model_a is None or model_c is None:
        joined = "; ".join(errors) if errors else "no readiness transport succeeded"
        print(f"ERROR: failed to query Bonsai readiness: {joined}", file=sys.stderr)
        raise SystemExit(1)

    print(f"source: {source}")
    print(format_check(model_a))
    print()
    print(format_check(model_c))

    if not (model_a.ready and model_c.ready):
        raise SystemExit(1)


if __name__ == "__main__":
    main()
