"""
BGP-LS bridge: polls gobgpd gRPC for BGP-LS NLRI, converts to Bonsai
BgpLsEvent JSON lines, and streams them to the Bonsai BGP-LS TCP listener.

Output format matches src/streaming/bgp_ls.rs BgpLsEvent enum variants:
  {"kind":"node", "router_id":"...", "protocol":"IS-IS", ...}
  {"kind":"link", "local_router_id":"...", "remote_router_id":"...", ...}

Reconnects to both gobgpd and bonsai on transient failures.
"""

import argparse
import json
import logging
import socket
import struct
import time
from typing import Optional

logging.basicConfig(level=logging.INFO, format="%(asctime)s %(levelname)s %(message)s")
log = logging.getLogger("bgp_ls_bridge")

# ---------------------------------------------------------------------------
# BGP-LS NLRI constants (RFC 7752)
# ---------------------------------------------------------------------------
NLRI_NODE = 1
NLRI_LINK = 2
NLRI_PREFIX_V4 = 3
NLRI_PREFIX_V6 = 4

PROTO_ISIS_L1 = 1
PROTO_ISIS_L2 = 2
PROTO_OSPF_V2 = 3
PROTO_OSPF_V3 = 6

PROTO_NAMES = {
    PROTO_ISIS_L1: "IS-IS-L1",
    PROTO_ISIS_L2: "IS-IS-L2",
    PROTO_OSPF_V2: "OSPF-v2",
    PROTO_OSPF_V3: "OSPF-v3",
}

# BGP-LS attribute TLV types (RFC 7752 §3.3)
TLV_NODE_NAME = 1026
TLV_SR_CAPS = 1034   # SR capabilities (contains node SID index)
TLV_TE_METRIC = 1092
TLV_IGP_METRIC = 1095
TLV_MAX_LINK_BW = 1081
TLV_UNRESERVED_BW = 1082
TLV_ADMIN_GROUP = 1088
TLV_SRLG = 1096


def now_ns() -> int:
    return int(time.time() * 1e9)


def _parse_router_id(data: bytes) -> str:
    if len(data) == 4:
        return ".".join(str(b) for b in data)
    if len(data) == 16:
        import ipaddress
        return str(ipaddress.IPv6Address(data))
    return data.hex()


def _parse_link_id_descriptor(data: bytes) -> dict:
    """Extract local/remote router-IDs from a BGP-LS Link NLRI descriptor TLV."""
    result = {}
    cursor = 0
    while cursor + 4 <= len(data):
        tlv_t = struct.unpack_from(">H", data, cursor)[0]
        tlv_l = struct.unpack_from(">H", data, cursor + 2)[0]
        cursor += 4
        if cursor + tlv_l > len(data):
            break
        v = data[cursor:cursor + tlv_l]
        cursor += tlv_l
        if tlv_t == 516:   # IPv4 router-ID of local node
            result["local_router_id"] = _parse_router_id(v)
        elif tlv_t == 517:  # IPv6 router-ID of local node
            result.setdefault("local_router_id", _parse_router_id(v))
        elif tlv_t == 518:  # IPv4 router-ID of remote node
            result["remote_router_id"] = _parse_router_id(v)
        elif tlv_t == 519:  # IPv6 router-ID of remote node
            result.setdefault("remote_router_id", _parse_router_id(v))
    return result


def _parse_link_attrs(data: bytes) -> dict:
    """Parse BGP-LS link attribute TLVs from NLRI attribute."""
    attrs: dict = {}
    cursor = 0
    while cursor + 4 <= len(data):
        tlv_t = struct.unpack_from(">H", data, cursor)[0]
        tlv_l = struct.unpack_from(">H", data, cursor + 2)[0]
        cursor += 4
        if cursor + tlv_l > len(data):
            break
        v = data[cursor:cursor + tlv_l]
        cursor += tlv_l
        if tlv_t == TLV_TE_METRIC and tlv_l == 4:
            attrs["te_metric"] = struct.unpack_from(">I", v)[0]
        elif tlv_t == TLV_IGP_METRIC:
            if tlv_l == 1:
                attrs["igp_metric"] = v[0]
            elif tlv_l == 2:
                attrs["igp_metric"] = struct.unpack_from(">H", v)[0]
            elif tlv_l == 3:
                attrs["igp_metric"] = int.from_bytes(v, "big")
        elif tlv_t == TLV_UNRESERVED_BW and tlv_l == 32:
            # 8 priority classes × 4-byte IEEE float; sum all as conservative total
            total = sum(struct.unpack_from(">f", v, i * 4)[0] for i in range(8))
            attrs["unreserved_bandwidth_bps"] = int(total * 8)
        elif tlv_t == TLV_SRLG:
            srlgs = [struct.unpack_from(">I", v, i * 4)[0] for i in range(tlv_l // 4)]
            attrs["srlgs"] = srlgs
    return attrs


def _parse_node_attrs(data: bytes) -> dict:
    attrs: dict = {}
    cursor = 0
    while cursor + 4 <= len(data):
        tlv_t = struct.unpack_from(">H", data, cursor)[0]
        tlv_l = struct.unpack_from(">H", data, cursor + 2)[0]
        cursor += 4
        if cursor + tlv_l > len(data):
            break
        v = data[cursor:cursor + tlv_l]
        cursor += tlv_l
        if tlv_t == TLV_NODE_NAME:
            attrs["name"] = v.decode("ascii", errors="replace")
    return attrs


def nlri_to_events(raw_nlri: bytes, raw_attrs: bytes, protocol: int) -> list[dict]:
    """Convert raw BGP-LS NLRI + attributes to a list of BgpLsEvent dicts."""
    if len(raw_nlri) < 4:
        return []
    nlri_type = struct.unpack_from(">H", raw_nlri, 0)[0]
    proto_name = PROTO_NAMES.get(protocol, f"unknown-{protocol}")
    ts = now_ns()

    if nlri_type == NLRI_NODE:
        node_attrs = _parse_node_attrs(raw_attrs)
        # Router-ID is in the local node descriptor sub-TLVs after the 4-byte protocol/id header
        # Simplified: treat the first 4 bytes after NLRI type+len as descriptor
        router_id = ""
        if len(raw_nlri) >= 8:
            try:
                router_id = _parse_router_id(raw_nlri[4:8])
            except Exception:
                pass
        return [{
            "kind": "node",
            "timestamp_ns": ts,
            "router_id": router_id,
            "protocol": proto_name,
            "name": node_attrs.get("name"),
            "sr_node_sid": node_attrs.get("sr_node_sid"),
        }]

    if nlri_type == NLRI_LINK:
        ids = _parse_link_id_descriptor(raw_nlri[4:])
        link_attrs = _parse_link_attrs(raw_attrs)
        event: dict = {
            "kind": "link",
            "timestamp_ns": ts,
            "local_router_id": ids.get("local_router_id", ""),
            "remote_router_id": ids.get("remote_router_id", ""),
            "protocol": proto_name,
        }
        event.update(link_attrs)
        return [event]

    return []


class BonsaiSender:
    def __init__(self, host: str, port: int):
        self.host = host
        self.port = port
        self._sock: Optional[socket.socket] = None

    def _connect(self):
        s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        s.settimeout(10)
        s.connect((self.host, self.port))
        self._sock = s
        log.info("Connected to Bonsai BGP-LS listener at %s:%d", self.host, self.port)

    def send(self, events: list[dict]):
        for ev in events:
            line = json.dumps(ev) + "\n"
            self._ensure_connected()
            assert self._sock is not None
            self._sock.sendall(line.encode())

    def _ensure_connected(self):
        if self._sock is None:
            self._connect()

    def close(self):
        if self._sock:
            try:
                self._sock.close()
            except OSError:
                pass
            self._sock = None


def poll_gobgp_and_bridge(gobgp_addr: str, sender: BonsaiSender):
    """
    Poll gobgpd JSON RIB for BGP-LS routes and bridge to Bonsai.
    Uses the gobgp CLI (subprocess) to dump the LS RIB as JSON, since
    the Python gRPC client for GoBGP requires generated stubs that we
    avoid bundling. The CLI is available in the same image.
    """
    import subprocess
    import shlex

    cmd = shlex.split(f"gobgp -u {gobgp_addr} global rib -a ls --format json")
    while True:
        try:
            result = subprocess.run(cmd, capture_output=True, text=True, timeout=15)
            if result.returncode != 0:
                log.warning("gobgp CLI error: %s", result.stderr.strip())
                time.sleep(5)
                continue
            routes = json.loads(result.stdout or "[]")
            events_batch = []
            for route in routes:
                # GoBGP JSON structure for LS routes
                nlri_raw = bytes.fromhex(route.get("nlri", {}).get("raw", ""))
                attrs_raw = bytes.fromhex(route.get("attrs_raw", ""))
                protocol = route.get("nlri", {}).get("protocol", 2)
                events_batch.extend(nlri_to_events(nlri_raw, attrs_raw, protocol))

            if events_batch:
                sender.send(events_batch)
                log.info("Bridged %d BGP-LS events to Bonsai", len(events_batch))

        except subprocess.TimeoutExpired:
            log.warning("gobgp CLI timed out")
        except json.JSONDecodeError as e:
            log.warning("Failed to parse gobgp JSON output: %s", e)
        except (OSError, ConnectionResetError) as e:
            log.warning("Bonsai connection lost: %s — reconnecting", e)
            sender.close()

        time.sleep(30)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--gobgp-addr", default="localhost:50052")
    parser.add_argument("--bonsai-addr", default="127.0.0.1")
    parser.add_argument("--bonsai-port", type=int, default=10179)
    args = parser.parse_args()

    sender = BonsaiSender(args.bonsai_addr, args.bonsai_port)
    log.info("BGP-LS bridge starting: gobgpd=%s bonsai=%s:%d",
             args.gobgp_addr, args.bonsai_addr, args.bonsai_port)
    try:
        poll_gobgp_and_bridge(args.gobgp_addr, sender)
    finally:
        sender.close()


if __name__ == "__main__":
    main()
