"""ServiceNow CMDB mock server.

Emulates the narrow subset of ServiceNow REST API that the bonsai enricher
uses. Not a full CMDB simulator — just the response shapes bonsai cares about.

Endpoints:
  POST /oauth_token.do                         — OAuth2 client_credentials
  GET  /api/now/table/cmdb_ci_netgear          — network device CIs
  GET  /api/now/table/cmdb_ci_business_service — business service CIs
  GET  /api/now/table/cmdb_rel_ci              — CI relationships
  GET  /health                                  — liveness probe

Usage:
  uvicorn app:app --host 0.0.0.0 --port 8080
"""
import os
from pathlib import Path
from typing import Any

import yaml
from fastapi import Depends, FastAPI, HTTPException, Query, Request
from fastapi.responses import JSONResponse
from fastapi.security import HTTPAuthorizationCredentials, HTTPBearer

SEED_FILE = Path(__file__).parent / "seed.yaml"

app = FastAPI(title="Bonsai ServiceNow CMDB Mock", version="0.1.0")
bearer = HTTPBearer(auto_error=False)

# ── Seed data ─────────────────────────────────────────────────────────────────

_seed: dict[str, Any] = {}


def load_seed() -> dict[str, Any]:
    global _seed
    if not _seed:
        with open(SEED_FILE) as f:
            _seed = yaml.safe_load(f)
    return _seed


# ── Auth ──────────────────────────────────────────────────────────────────────

VALID_TOKEN = "mock-sn-token"


@app.post("/oauth_token.do")
async def oauth_token(request: Request):
    """Accept any well-formed client_credentials request and return a mock token."""
    form = await request.form()
    if form.get("grant_type") != "client_credentials":
        raise HTTPException(status_code=400, detail="grant_type must be client_credentials")
    return {
        "access_token": VALID_TOKEN,
        "token_type": "Bearer",
        "expires_in": 3600,
        "scope": "useraccount",
    }


def require_auth(creds: HTTPAuthorizationCredentials = Depends(bearer)):
    if creds is None or creds.credentials != VALID_TOKEN:
        raise HTTPException(status_code=401, detail="Invalid or missing Bearer token")
    return creds


# ── Helper ────────────────────────────────────────────────────────────────────

def paginate(records: list, sysparm_limit: int = 50, sysparm_offset: int = 0) -> dict:
    sliced = records[sysparm_offset: sysparm_offset + sysparm_limit]
    return {"result": sliced}


# ── Table endpoints ───────────────────────────────────────────────────────────

@app.get("/api/now/table/cmdb_ci_netgear")
async def get_network_cis(
    sysparm_limit: int = Query(50),
    sysparm_offset: int = Query(0),
    name: str = Query(None),
    _auth=Depends(require_auth),
):
    seed = load_seed()
    records = seed.get("cmdb_ci_netgear", [])
    if name:
        records = [r for r in records if r.get("name") == name]
    return paginate(records, sysparm_limit, sysparm_offset)


@app.get("/api/now/table/cmdb_ci_business_service")
async def get_business_services(
    sysparm_limit: int = Query(50),
    sysparm_offset: int = Query(0),
    _auth=Depends(require_auth),
):
    seed = load_seed()
    records = seed.get("cmdb_ci_business_service", [])
    return paginate(records, sysparm_limit, sysparm_offset)


@app.get("/api/now/table/cmdb_rel_ci")
async def get_ci_relationships(
    sysparm_limit: int = Query(50),
    sysparm_offset: int = Query(0),
    parent: str = Query(None),
    child: str = Query(None),
    _auth=Depends(require_auth),
):
    seed = load_seed()
    records = seed.get("cmdb_rel_ci", [])
    if parent:
        records = [r for r in records if r.get("parent") == parent]
    if child:
        records = [r for r in records if r.get("child") == child]
    return paginate(records, sysparm_limit, sysparm_offset)


# ── Health ────────────────────────────────────────────────────────────────────

@app.get("/health")
async def health():
    seed = load_seed()
    return {
        "status": "ok",
        "devices": len(seed.get("cmdb_ci_netgear", [])),
        "services": len(seed.get("cmdb_ci_business_service", [])),
        "relationships": len(seed.get("cmdb_rel_ci", [])),
    }
