#!/usr/bin/env python3
"""Quick smoke-test for the Bonsai Python SDK.

Run with bonsai already started:
    python python/example.py
"""
from bonsai_sdk import BonsaiClient

def main():
    with BonsaiClient("[::1]:50051") as c:
        print("=== Devices ===")
        for d in c.get_devices():
            print(f"  {d.address}  vendor={d.vendor}  hostname={d.hostname}")

        print("\n=== BGP Neighbors ===")
        for n in c.get_bgp_neighbors():
            print(f"  {n.device_address} -> {n.peer_address}  AS{n.peer_as}  {n.session_state}")

        print("\n=== Topology edges ===")
        for e in c.get_topology():
            print(f"  {e.src_device}:{e.src_interface} -> {e.dst_device}:{e.dst_interface}")

        print("\n=== Raw Cypher query ===")
        rows = c.query("MATCH (n:Device) RETURN n.hostname, n.vendor")
        for row in rows:
            print(f"  {row}")

if __name__ == "__main__":
    main()
