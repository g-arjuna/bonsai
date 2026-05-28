"""Device config/CLI text embedding background worker for Bonsai ML.

EV1-3 T2: Polls GET /api/devices/unembedded-config?limit=50 for devices
with needs_config_embedding=true. For each device, fetches config text from
GET /api/devices/{address}/config-text, embeds it, and POSTs to
POST /api/devices/{address}/config-embedding.

These DeviceConfigEmbedding nodes are separate from the topology spectral
embeddings (DeviceEmbedding). They capture semantic config similarity:
two Nokia SRL devices with identical EVPN configs cluster together,
even if their topology positions differ.

Run as:
    python -m bonsai_ml.config_embedding_worker
"""
from __future__ import annotations

import logging
import os
import time
from typing import Any

log = logging.getLogger(__name__)

POLL_INTERVAL_SECS = 300
FETCH_LIMIT = 50
BACKOFF_SECS = [60, 120, 300]
DEFAULT_API_URL = "http://localhost:3000"


def run_config_embedding_worker(
    api_url: str = DEFAULT_API_URL,
    embedding_config: dict[str, Any] | None = None,
    poll_interval: int = POLL_INTERVAL_SECS,
    run_once: bool = False,
) -> None:
    """Run the device config embedding background worker loop.

    Args:
        api_url: Bonsai core API base URL.
        embedding_config: Dict passed to load_from_config(). None = defaults.
        poll_interval: Seconds between polling cycles (default 5 minutes).
        run_once: If True, run one cycle and exit.
    """
    from .text_embeddings import load_from_config

    embedder = load_from_config(embedding_config or {})
    log.info(
        "ConfigEmbeddingWorker: starting (model=%s, poll_interval=%ds)",
        embedder.model_name, poll_interval,
    )

    backoff_idx = 0
    total_embedded = 0

    while True:
        try:
            devices = _fetch_unembedded_devices(api_url, limit=FETCH_LIMIT)

            if devices:
                embedded_count = _embed_devices(api_url, devices, embedder)
                total_embedded += embedded_count
                log.info(
                    "ConfigEmbeddingWorker: embedded %d devices (total=%d)",
                    embedded_count, total_embedded,
                )
                backoff_idx = 0
            else:
                log.debug("ConfigEmbeddingWorker: no devices with pending config embeddings")

        except Exception as exc:
            sleep = BACKOFF_SECS[min(backoff_idx, len(BACKOFF_SECS) - 1)]
            log.error(
                "ConfigEmbeddingWorker: error (backing off %ds): %s", sleep, exc
            )
            backoff_idx += 1
            if run_once:
                raise
            time.sleep(sleep)
            continue

        if run_once:
            break

        time.sleep(poll_interval)


def _fetch_unembedded_devices(api_url: str, limit: int) -> list[dict[str, Any]]:
    try:
        import requests
        resp = requests.get(
            f"{api_url}/api/devices/unembedded-config",
            params={"limit": limit},
            timeout=15,
        )
        resp.raise_for_status()
        data = resp.json()
        return data.get("devices", data) if isinstance(data, dict) else data
    except Exception as exc:
        log.debug("Failed to fetch unembedded devices: %s", exc)
        return []


def _fetch_config_text(api_url: str, address: str) -> str:
    try:
        import requests
        resp = requests.get(
            f"{api_url}/api/devices/{address}/config-text",
            timeout=15,
        )
        if resp.ok:
            data = resp.json()
            return data.get("config_text") or data.get("text") or ""
    except Exception as exc:
        log.debug("Failed to fetch config text for %s: %s", address, exc)
    return ""


def _post_config_embedding(
    api_url: str,
    address: str,
    vector: list[float],
    model_name: str,
    dim: int,
    schema_hash: str,
) -> bool:
    try:
        import requests
        resp = requests.post(
            f"{api_url}/api/devices/{address}/config-embedding",
            json={
                "vector": vector,
                "model_name": model_name,
                "dim": dim,
                "computed_at_ns": time.time_ns(),
                "schema_hash": schema_hash,
            },
            timeout=15,
        )
        return resp.ok
    except Exception as exc:
        log.debug("Failed to POST config embedding for %s: %s", address, exc)
        return False


def _embed_devices(
    api_url: str,
    devices: list[dict[str, Any]],
    embedder: Any,
) -> int:
    from .feature_schema import DEVICE_V2_SCHEMA

    schema_hash = DEVICE_V2_SCHEMA.schema_hash
    embedded = 0

    for device in devices:
        address = device.get("address") or device.get("id") or ""
        if not address:
            continue

        config_text = _fetch_config_text(api_url, address)
        if not config_text.strip():
            log.debug("No config text for device %s, skipping", address)
            continue

        try:
            vector = embedder.embed_single(config_text).tolist()
            success = _post_config_embedding(
                api_url=api_url,
                address=address,
                vector=vector,
                model_name=embedder.model_name,
                dim=embedder.dim,
                schema_hash=schema_hash,
            )
            if success:
                embedded += 1
                log.debug("Config embedding posted for %s", address)
        except Exception as exc:
            log.warning("Config embedding failed for %s: %s", address, exc)

    return embedded


if __name__ == "__main__":
    logging.basicConfig(level=logging.INFO)
    api_url = os.environ.get("BONSAI_API_URL", DEFAULT_API_URL)
    run_config_embedding_worker(api_url=api_url)
