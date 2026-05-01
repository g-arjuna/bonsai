"""High-level Python client for the Bonsai gRPC API."""
from __future__ import annotations

import json
import sys
import urllib.error
import urllib.request
from pathlib import Path
from typing import Generator, Iterator

import grpc

# Allow running from the repo root without installing the package
sys.path.insert(0, str(Path(__file__).parent.parent))
sys.path.insert(0, str(Path(__file__).parent.parent / "generated"))
from generated import bonsai_service_pb2 as pb
from generated import bonsai_service_pb2_grpc as pb_grpc


class BonsaiClient:
    """Synchronous gRPC client for BonsaiGraph.

    Usage::

        with BonsaiClient() as c:
            for dev in c.get_devices():
                print(dev.hostname, dev.vendor)
    """

    def __init__(
        self,
        address: str = "[::1]:50051",
        *,
        ca_cert: str | None = None,
        cert: str | None = None,
        key: str | None = None,
        server_name: str | None = None,
        http_base_url: str = "http://127.0.0.1:3000",
    ):
        self._address = address
        self._ca_cert = ca_cert
        self._cert = cert
        self._key = key
        self._server_name = server_name
        self._http_base_url = http_base_url.rstrip("/")
        self._channel: grpc.Channel | None = None
        self._stub: pb_grpc.BonsaiGraphStub | None = None

    # ── context manager ───────────────────────────────────────────────────────

    def __enter__(self) -> "BonsaiClient":
        self.connect()
        return self

    def __exit__(self, *_) -> None:
        self.close()

    def connect(self) -> None:
        # keepalive: ping every 30s, timeout after 10s, allow pings without calls
        options = [
            ("grpc.keepalive_time_ms",              30_000),
            ("grpc.keepalive_timeout_ms",           10_000),
            ("grpc.keepalive_permit_without_calls",      1),
            ("grpc.http2.max_pings_without_data",        0),
        ]
        if self._server_name:
            options.append(("grpc.ssl_target_name_override", self._server_name))

        if self._ca_cert and self._cert and self._key:
            with open(self._ca_cert, "rb") as f:
                root_certs = f.read()
            with open(self._cert, "rb") as f:
                cert_chain = f.read()
            with open(self._key, "rb") as f:
                private_key = f.read()
            
            creds = grpc.ssl_channel_credentials(
                root_certificates=root_certs,
                private_key=private_key,
                certificate_chain=cert_chain,
            )
            self._channel = grpc.secure_channel(self._address, creds, options=options)
        else:
            self._channel = grpc.insecure_channel(self._address, options=options)
        
        self._stub = pb_grpc.BonsaiGraphStub(self._channel)

    def close(self) -> None:
        if self._channel:
            self._channel.close()
            self._channel = None
            self._stub = None

    # ── helpers ───────────────────────────────────────────────────────────────

    @property
    def stub(self) -> pb_grpc.BonsaiGraphStub:
        if self._stub is None:
            raise RuntimeError("Not connected — use BonsaiClient as a context manager or call connect()")
        return self._stub

    # ── typed RPCs ────────────────────────────────────────────────────────────

    def query(self, cypher: str) -> list[list]:
        """Execute a raw Cypher query; returns a list of rows (each row is a list)."""
        resp = self.stub.Query(pb.QueryRequest(cypher=cypher))
        if resp.error:
            raise RuntimeError(f"Query error: {resp.error}")
        return json.loads(resp.json_rows) if resp.json_rows else []

    def get_devices(self) -> list:
        """Return all Device nodes."""
        return list(self.stub.GetDevices(pb.GetDevicesRequest()).devices)

    def get_interfaces(self, device_address: str = "") -> list:
        """Return Interface nodes, optionally filtered by device address."""
        req = pb.GetInterfacesRequest(device_address=device_address)
        return list(self.stub.GetInterfaces(req).interfaces)

    def get_bgp_neighbors(self, device_address: str = "") -> list:
        """Return BgpNeighbor nodes, optionally filtered by device address."""
        req = pb.GetBgpNeighborsRequest(device_address=device_address)
        return list(self.stub.GetBgpNeighbors(req).neighbors)

    def get_topology(self) -> list:
        """Return CONNECTED_TO topology edges."""
        return list(self.stub.GetTopology(pb.GetTopologyRequest()).edges)

    def list_sites(self) -> list:
        """Return Site graph nodes used by onboarding and topology grouping."""
        return list(self.stub.ListSites(pb.ListSitesRequest()).sites)

    def add_site(
        self,
        name: str,
        *,
        site_id: str = "",
        parent_id: str = "",
        kind: str = "unknown",
        lat: float = 0.0,
        lon: float = 0.0,
        metadata_json: str = "{}",
    ):
        """Create or update a Site node. Empty site_id is slugged by the server."""
        resp = self.stub.AddSite(
            pb.AddSiteRequest(
                site=pb.Site(
                    id=site_id,
                    name=name,
                    parent_id=parent_id,
                    kind=kind,
                    lat=lat,
                    lon=lon,
                    metadata_json=metadata_json,
                )
            )
        )
        if not resp.success:
            raise RuntimeError(f"AddSite error: {resp.error}")
        return resp.site

    def list_credentials(self) -> list:
        """Return credential aliases and metadata only; secrets never leave Rust."""
        return list(self.stub.ListCredentials(pb.ListCredentialsRequest()).credentials)

    def add_credential(self, alias: str, username: str, password: str):
        """Store or update a credential alias in the local encrypted vault."""
        resp = self.stub.AddCredential(
            pb.AddCredentialRequest(alias=alias, username=username, password=password)
        )
        if not resp.success:
            raise RuntimeError(f"AddCredential error: {resp.error}")
        return resp.credential

    def remove_credential(self, alias: str):
        """Remove a credential alias from the local encrypted vault."""
        resp = self.stub.RemoveCredential(pb.RemoveCredentialRequest(alias=alias))
        if not resp.success:
            raise RuntimeError(f"RemoveCredential error: {resp.error}")
        return resp.credential

    def create_detection(
        self,
        device_address: str,
        rule_id: str,
        severity: str,
        features_json: str,
        fired_at_ns: int,
        state_change_event_id: str = "",
    ):
        req = pb.CreateDetectionRequest(
            device_address=device_address,
            rule_id=rule_id,
            severity=severity,
            features_json=features_json,
            fired_at_ns=fired_at_ns,
            state_change_event_id=state_change_event_id,
        )
        resp = self.stub.CreateDetection(req)
        if resp.error:
            raise RuntimeError(f"CreateDetection error: {resp.error}")
        return resp

    def create_remediation(
        self,
        detection_id: str,
        action: str,
        status: str,
        detail_json: str,
        attempted_at_ns: int,
        completed_at_ns: int = 0,
    ):
        req = pb.CreateRemediationRequest(
            detection_id=detection_id,
            action=action,
            status=status,
            detail_json=detail_json,
            attempted_at_ns=attempted_at_ns,
            completed_at_ns=completed_at_ns,
        )
        resp = self.stub.CreateRemediation(req)
        if resp.error:
            raise RuntimeError(f"CreateRemediation error: {resp.error}")
        return resp

    def push_remediation(self, target_address: str, yang_path: str, json_value: str):
        """Execute a gNMI Set on a managed device. Credentials stay in the Rust process."""
        req = pb.PushRemediationRequest(
            target_address=target_address,
            yang_path=yang_path,
            json_value=json_value,
        )
        return self.stub.PushRemediation(req)

    def create_remediation_proposal(
        self,
        *,
        detection_id: str,
        playbook_id: str,
        rule_id: str,
        environment_archetype: str,
        site_id: str,
        steps_json: str,
        rollback_steps_json: str = "[]",
    ) -> dict:
        return self._http_json(
            "POST",
            "/api/approvals",
            {
                "detection_id": detection_id,
                "playbook_id": playbook_id,
                "rule_id": rule_id,
                "environment_archetype": environment_archetype,
                "site_id": site_id,
                "steps_json": steps_json,
                "rollback_steps_json": rollback_steps_json,
            },
        )

    def approve_remediation_proposal(self, proposal_id: str, operator_note: str = "") -> dict:
        return self._http_json(
            "POST",
            f"/api/approvals/{proposal_id}/approve",
            {"operator_note": operator_note},
        )

    def reject_remediation_proposal(self, proposal_id: str, operator_note: str = "") -> dict:
        return self._http_json(
            "POST",
            f"/api/approvals/{proposal_id}/reject",
            {"operator_note": operator_note},
        )

    def _http_json(self, method: str, path: str, payload: dict | None = None) -> dict:
        data = None if payload is None else json.dumps(payload).encode("utf-8")
        req = urllib.request.Request(
            f"{self._http_base_url}{path}",
            data=data,
            method=method,
            headers={"Content-Type": "application/json"},
        )
        try:
            with urllib.request.urlopen(req, timeout=10) as resp:
                body = resp.read().decode("utf-8")
                return json.loads(body) if body else {}
        except urllib.error.HTTPError as exc:
            body = exc.read().decode("utf-8", errors="replace")
            raise RuntimeError(f"HTTP {exc.code} {path}: {body}") from exc

    def detection_ingest(self, events: Generator[pb.DetectionEventIngest, None, None]):
        """Client-streaming: push locally-evaluated detections to core."""
        return self.stub.DetectionIngest(events)

    def stream_events(
        self,
        event_types: list[str] | None = None,
        device_address: str = "",
    ) -> Iterator:
        """Server-streaming: yields StateEvent messages as they arrive.

        Args:
            event_types: filter to specific event types (e.g. ["bgp_session_change"]).
                         Empty/None means all types.
            device_address: filter to a single device. Empty means all devices.
        """
        req = pb.StreamEventsRequest(
            event_types=event_types or [],
            device_address=device_address,
        )
        return self.stub.StreamEvents(req)
