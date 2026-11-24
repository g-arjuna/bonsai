"""
PyATS + TextFSM sidecar — real CLI parsing for Bonsai.

Parsing chain:
  1. Genie parser (structured, vendor-aware) — preferred
  2. TextFSM via ntc-templates — fallback for commands Genie can't parse
  3. Line-split — last resort

POST /parse   — parse raw CLI output for a (vendor, command) pair
POST /learn   — SSH to a device and run Genie learn() for a feature set
GET  /healthz — health check
"""
from __future__ import annotations

import logging
import os
import traceback
from typing import Any

from fastapi import FastAPI
from pydantic import BaseModel

logger = logging.getLogger("pyats-sidecar")
logging.basicConfig(level=logging.INFO, format="%(asctime)s %(levelname)s %(message)s")

app = FastAPI(title="Bonsai PyATS Sidecar")

# ── Lazy imports (heavy — only load on first call) ────────────────────────────

_genie_loaded = False
_textfsm_loaded = False
_genie_parse = None
_textfsm_index = None


def _ensure_genie():
    global _genie_loaded, _genie_parse
    if _genie_loaded:
        return
    try:
        from genie.libs.parser.utils import get_parser
        _genie_parse = get_parser
        logger.info("Genie parser library loaded")
    except ImportError:
        logger.warning("Genie not installed — Genie parsing unavailable")
        _genie_parse = None
    _genie_loaded = True


def _ensure_textfsm():
    global _textfsm_loaded, _textfsm_index
    if _textfsm_loaded:
        return
    try:
        from ntc_templates.parse import parse_output
        _textfsm_index = parse_output
        logger.info("ntc-templates TextFSM library loaded")
    except ImportError:
        logger.warning("ntc-templates not installed — TextFSM parsing unavailable")
        _textfsm_index = None
    _textfsm_loaded = True


# ── Vendor mapping ────────────────────────────────────────────────────────────

# Bonsai vendor string → Genie OS name
VENDOR_TO_GENIE_OS = {
    "cisco_iosxe": "iosxe",
    "cisco_iosxr": "iosxr",
    "cisco_nxos": "nxos",
    "arista_eos": "eos",
    "juniper_junos": "junos",
    # nokia_srlinux: No Genie/Unicon plugin exists for SR Linux.
    # SRL bootstrap uses paramiko-native path in bootstrap_agent.py.
    "nokia_sros": "sros",
    "frr": "linux",
}

# Bonsai vendor string → ntc-templates platform
VENDOR_TO_NTC_PLATFORM = {
    "cisco_iosxe": "cisco_ios",
    "cisco_iosxr": "cisco_xr",
    "cisco_nxos": "cisco_nxos",
    "arista_eos": "arista_eos",
    "juniper_junos": "juniper_junos",
    "nokia_srlinux": "nokia_srl",
    "nokia_sros": "nokia_sros",
    "frr": "linux",
}


# ── Request / response models ────────────────────────────────────────────────

class ParseRequest(BaseModel):
    parser: str = "auto"  # "genie", "textfsm", "auto" (try genie first)
    vendor: str
    command: str  # full CLI command, e.g. "show ip route"
    raw_output: str


GENIE_FEATURES_ALL = [
    "interface", "bgp", "lldp", "lag", "vrrp", "routing", "arp",
    "ospf", "isis", "bfd", "stp", "vlan", "vrf", "ntp", "platform", "acl", "mpls",
]

# Topology-aware feature profiles: use `profile` field instead of listing features
TOPOLOGY_PROFILES = {
    "dc_leaf": ["interface", "bgp", "lldp", "vlan", "vrf", "lag", "bfd", "vrrp", "platform", "acl"],
    "dc_spine": ["interface", "bgp", "lldp", "lag", "bfd", "platform"],
    "campus_access": ["interface", "lldp", "stp", "vlan", "vrrp", "arp", "ospf", "ntp", "acl", "platform"],
    "campus_core": ["interface", "bgp", "ospf", "lldp", "lag", "bfd", "vrf", "ntp", "platform"],
    "campus_distribution": ["interface", "ospf", "lldp", "stp", "vlan", "vrf", "lag", "vrrp", "ntp", "acl", "platform"],
    "sp_pe": ["interface", "bgp", "isis", "mpls", "lldp", "bfd", "vrf", "lag", "platform"],
    "sp_p": ["interface", "isis", "mpls", "lldp", "bfd", "lag", "platform"],
    "homelab": ["interface", "bgp", "lldp", "routing", "arp", "ntp", "platform"],
    "full": GENIE_FEATURES_ALL,
}


class LearnRequest(BaseModel):
    address: str
    username: str
    password: str
    vendor: str = ""
    features: list[str] = GENIE_FEATURES_ALL
    profile: str = ""  # optional: dc_leaf, campus_access, sp_pe, etc.
    port: int = 22


# ── Parsing backends ─────────────────────────────────────────────────────────
#
# Each backend reports a distinct outcome so empty results, unsupported
# commands, unavailable dependencies and execution failures never collapse
# into the same "try the next thing" signal:
#
#   structured_success  — parser ran and produced records
#   structured_empty    — parser ran and legitimately produced zero records
#                         (e.g. an empty but valid BGP table)
#   unsupported         — no parser/template exists for this vendor+command
#   backend_unavailable — the parser library itself is not installed
#   parser_error        — the parser ran and failed
#
# Only structured_* outcomes may surface as backend_used "genie"/"textfsm".
# Everything else degrades to the explicit line-split fallback, whose outcome
# is "unstructured_fallback" — never a silent success.

OUTCOME_STRUCTURED_SUCCESS = "structured_success"
OUTCOME_STRUCTURED_EMPTY = "structured_empty"
OUTCOME_UNSUPPORTED = "unsupported"
OUTCOME_BACKEND_UNAVAILABLE = "backend_unavailable"
OUTCOME_PARSER_ERROR = "parser_error"
OUTCOME_UNSTRUCTURED_FALLBACK = "unstructured_fallback"
OUTCOME_INVALID_INPUT = "invalid_input"


def _sanitize_error(exc: Exception) -> str:
    """Bounded diagnostic: exception type + truncated message. Never includes
    raw device output or credentials."""
    return f"{type(exc).__name__}: {str(exc)[:200]}"


def run_genie(vendor: str, command: str, raw_output: str):
    """Returns (outcome, payload, detail)."""
    _ensure_genie()
    if _genie_parse is None:
        return OUTCOME_BACKEND_UNAVAILABLE, None, "genie library not installed"
    genie_os = VENDOR_TO_GENIE_OS.get(vendor.lower())
    if not genie_os:
        return OUTCOME_UNSUPPORTED, None, f"no genie os mapping for vendor '{vendor}'"
    try:
        from genie.libs.parser.utils.common import ParserNotFound
        from unittest.mock import MagicMock

        # Genie get_parser(command, device) resolves the parser class and
        # kwargs from device tokens; the parser then parses supplied output
        # offline via cli(output=...). A MagicMock device supplies only the
        # os/platform attributes token lookup needs — no connection is made.
        device = MagicMock()
        device.os = genie_os
        device.platform = ""
        device.custom = {}

        parser_class, parser_kwargs = _genie_parse(command, device)
        parser = parser_class(device=device)
        result = parser.cli(output=raw_output, **parser_kwargs)
    except ParserNotFound as e:
        return OUTCOME_UNSUPPORTED, None, _sanitize_error(e)
    except Exception as e:
        logger.debug("Genie parse failed for %s/%s: %s", vendor, command, e)
        return OUTCOME_PARSER_ERROR, None, _sanitize_error(e)

    if result is None:
        return OUTCOME_PARSER_ERROR, None, "genie parser returned no result"
    payload = _serialize_genie(dict(result))
    outcome = OUTCOME_STRUCTURED_EMPTY if not payload else OUTCOME_STRUCTURED_SUCCESS
    return outcome, payload, ""


def run_textfsm(vendor: str, command: str, raw_output: str):
    """Returns (outcome, payload, detail)."""
    _ensure_textfsm()
    if _textfsm_index is None:
        return OUTCOME_BACKEND_UNAVAILABLE, None, "ntc-templates library not installed"
    platform = VENDOR_TO_NTC_PLATFORM.get(vendor.lower())
    if not platform:
        return OUTCOME_UNSUPPORTED, None, f"no ntc-templates platform for vendor '{vendor}'"
    try:
        from ntc_templates.parse import ParsingException

        result = _textfsm_index(platform=platform, command=command, data=raw_output)
    except ParsingException as e:
        message = str(e)
        if "No template found" in message:
            return OUTCOME_UNSUPPORTED, None, _sanitize_error(e)
        logger.debug("TextFSM parse failed for %s/%s: %s", vendor, command, e)
        return OUTCOME_PARSER_ERROR, None, _sanitize_error(e)
    except Exception as e:
        logger.debug("TextFSM parse failed for %s/%s: %s", vendor, command, e)
        return OUTCOME_PARSER_ERROR, None, _sanitize_error(e)

    if result is None:
        return OUTCOME_PARSER_ERROR, None, "textfsm parser returned no result"
    payload = _serialize_genie(list(result))
    outcome = OUTCOME_STRUCTURED_EMPTY if not payload else OUTCOME_STRUCTURED_SUCCESS
    return outcome, payload, ""


def fallback_parse(raw_output: str) -> dict:
    """Last resort: bounded line-split. Always labeled unstructured."""
    lines = [line.strip() for line in raw_output.splitlines() if line.strip()]
    return {
        "result_kind": "unstructured_fallback",
        "backend_used": "line_split",
        "line_count": len(lines),
        "lines": lines[:1000],
        "truncated": len(lines) > 1000,
    }


# ── Endpoints ────────────────────────────────────────────────────────────────

@app.get("/healthz")
def healthz() -> dict:
    _ensure_genie()
    _ensure_textfsm()
    return {
        "ok": True,
        "service": "pyats-sidecar",
        "genie_available": _genie_parse is not None,
        "textfsm_available": _textfsm_index is not None,
        "features": GENIE_FEATURES_ALL,
        "profiles": list(TOPOLOGY_PROFILES.keys()),
    }


def _respond(request: ParseRequest, backend_used: str, outcome: str,
             parsed: Any, detail: str = "") -> dict:
    response = {
        "parser": request.parser,
        "backend_used": backend_used,
        "outcome": outcome,
        "vendor": request.vendor,
        "command": request.command,
        "parsed_json": parsed,
    }
    if detail:
        response["detail"] = detail
    return response


@app.post("/parse")
def parse(request: ParseRequest) -> dict:
    """Parse raw CLI output: Genie → TextFSM → explicit line-split fallback.

    backend_used is "genie"/"textfsm" only when that backend actually
    produced the payload; "fallback" covers unsupported, failed, unavailable
    and invalid-input cases. `outcome` distinguishes structured_success,
    structured_empty, unstructured_fallback and invalid_input.
    """
    if not (request.raw_output or "").strip():
        return _respond(
            request, "fallback", OUTCOME_INVALID_INPUT,
            fallback_parse(request.raw_output or ""),
            detail="empty raw_output",
        )

    details: list[str] = []
    if request.parser in ("genie", "auto"):
        outcome, payload, detail = run_genie(
            request.vendor, request.command, request.raw_output)
        if outcome in (OUTCOME_STRUCTURED_SUCCESS, OUTCOME_STRUCTURED_EMPTY):
            return _respond(request, "genie", outcome, payload)
        details.append(f"genie={outcome}" + (f"({detail})" if detail else ""))

    if request.parser in ("textfsm", "auto"):
        outcome, payload, detail = run_textfsm(
            request.vendor, request.command, request.raw_output)
        if outcome in (OUTCOME_STRUCTURED_SUCCESS, OUTCOME_STRUCTURED_EMPTY):
            return _respond(request, "textfsm", outcome, payload)
        details.append(f"textfsm={outcome}" + (f"({detail})" if detail else ""))

    return _respond(
        request, "fallback", OUTCOME_UNSTRUCTURED_FALLBACK,
        fallback_parse(request.raw_output),
        detail="; ".join(details),
    )


@app.post("/learn")
def learn(request: LearnRequest) -> dict:
    """
    SSH to a device and run Genie learn() for the requested feature set.
    Returns structured data per feature.

    This is the primary entry point for device onboarding — the bootstrap agent
    calls this instead of running Genie locally, so parsing happens inside the
    sidecar container where all dependencies are installed.
    """
    _ensure_genie()
    if _genie_parse is None:
        return {"success": False, "error": "Genie not installed in sidecar"}

    try:
        from genie.testbed import load as genie_load
    except ImportError:
        return {"success": False, "error": "genie.testbed not available"}

    vendor = request.vendor.lower()
    if vendor in ("nokia_srlinux", "nokia_srl"):
        return {
            "success": False,
            "error": "SR Linux has no PyATS/Unicon plugin. Use bootstrap_agent.py paramiko-native path instead.",
        }
    genie_os = VENDOR_TO_GENIE_OS.get(vendor, "iosxe")

    # Resolve topology profile → feature list (profile overrides explicit features)
    if request.profile and request.profile in TOPOLOGY_PROFILES:
        features_to_learn = TOPOLOGY_PROFILES[request.profile]
    else:
        features_to_learn = request.features

    testbed_dict = {
        "devices": {
            request.address: {
                "os": genie_os,
                "type": "router",
                "credentials": {
                    "default": {
                        "username": request.username,
                        "password": request.password,
                    }
                },
                "connections": {
                    "default": {
                        "protocol": "ssh",
                        "ip": request.address,
                        "port": request.port,
                    }
                },
            }
        }
    }

    try:
        testbed = genie_load(testbed_dict)
        device = testbed.devices[request.address]
        device.connect(log_stdout=False)
    except Exception as e:
        return {"success": False, "error": f"SSH connect failed: {e}"}

    results: dict[str, Any] = {}
    for feature in features_to_learn:
        try:
            data = device.learn(feature)
            info = data.info if hasattr(data, "info") else {}
            results[feature] = {
                "success": True,
                "data": _serialize_genie(info),
            }
        except Exception as e:
            results[feature] = {
                "success": False,
                "error": str(e),
            }

    try:
        device.disconnect()
    except Exception:
        pass

    return {
        "success": True,
        "address": request.address,
        "profile": request.profile or "custom",
        "features_requested": len(features_to_learn),
        "features": results,
    }


def _serialize_genie(obj: Any) -> Any:
    """Recursively convert Genie output to JSON-serializable form."""
    if isinstance(obj, dict):
        return {str(k): _serialize_genie(v) for k, v in obj.items()}
    if isinstance(obj, (list, tuple)):
        return [_serialize_genie(v) for v in obj]
    if isinstance(obj, (int, float, str, bool, type(None))):
        return obj
    return str(obj)


# ── Main ─────────────────────────────────────────────────────────────────────

if __name__ == "__main__":
    import uvicorn
    port = int(os.environ.get("SIDECAR_PORT", "9101"))
    uvicorn.run(app, host="0.0.0.0", port=port)
