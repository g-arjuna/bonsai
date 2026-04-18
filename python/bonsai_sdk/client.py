"""High-level Python client for the Bonsai gRPC API."""
from __future__ import annotations

import json
import sys
from pathlib import Path
from typing import Generator, Iterator

import grpc

# Allow running from the repo root without installing the package
sys.path.insert(0, str(Path(__file__).parent.parent))
from generated import bonsai_service_pb2 as pb
from generated import bonsai_service_pb2_grpc as pb_grpc


class BonsaiClient:
    """Synchronous gRPC client for BonsaiGraph.

    Usage::

        with BonsaiClient() as c:
            for dev in c.get_devices():
                print(dev.hostname, dev.vendor)
    """

    def __init__(self, address: str = "[::1]:50051"):
        self._address = address
        self._channel: grpc.Channel | None = None
        self._stub: pb_grpc.BonsaiGraphStub | None = None

    # ── context manager ───────────────────────────────────────────────────────

    def __enter__(self) -> "BonsaiClient":
        self.connect()
        return self

    def __exit__(self, *_) -> None:
        self.close()

    def connect(self) -> None:
        self._channel = grpc.insecure_channel(self._address)
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
