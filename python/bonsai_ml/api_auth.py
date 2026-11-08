"""Shared Bonsai API credential helper (BN-009).

Sidecar ML workers, export jobs, and embedding loops call the JWT-protected
Bonsai HTTP API. When ``BONSAI_API_TOKEN`` is set (a ``bsk_`` API key minted
via ``POST /api/auth/apikeys``), every request carries
``Authorization: Bearer <token>``. When unset, requests stay unauthenticated
— the legacy dev mode where ``require_auth`` is disabled.

Usage::

    from bonsai_ml.api_auth import auth_headers
    requests.get(url, headers=auth_headers())
"""
from __future__ import annotations

import os
from typing import Optional

ENV_VAR = "BONSAI_API_TOKEN"


def api_token() -> str:
    """Return the configured API token, or "" when unset."""
    return os.environ.get(ENV_VAR, "").strip()


def auth_headers() -> dict[str, str]:
    """Return ``{"Authorization": "Bearer <token>"}`` or an empty dict."""
    token = api_token()
    return {"Authorization": f"Bearer {token}"} if token else {}


def merge_headers(headers: Optional[dict[str, str]] = None) -> dict[str, str]:
    """Merge auth headers into an existing header dict (auth wins on conflict)."""
    merged = dict(headers or {})
    merged.update(auth_headers())
    return merged
