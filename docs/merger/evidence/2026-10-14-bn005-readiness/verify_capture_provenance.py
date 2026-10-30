"""Compare complete BMP frames from a live-labelled file with known fixtures."""
import argparse
import hashlib
import json
from pathlib import Path


def split_frames(data):
    frames = []
    offset = 0
    while offset < len(data):
        if len(data) - offset < 6:
            raise ValueError("incomplete BMP header")
        length = int.from_bytes(data[offset + 1:offset + 5], "big")
        if data[offset] != 3 or length < 6 or offset + length > len(data):
            raise ValueError("invalid BMP framing")
        frames.append(data[offset:offset + length])
        offset += length
    return frames


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("bundle", type=Path)
    args = parser.parse_args()
    live = split_frames((args.bundle / "frr_live_network_capture.bin").read_bytes())
    fixtures = split_frames((args.bundle / "synthetic_vrf_fixture.bin").read_bytes())
    matches = [
        {"type": frame[5], "bytes": len(frame),
         "sha256": hashlib.sha256(frame).hexdigest()}
        for frame in live if frame in fixtures
    ]
    print(json.dumps({"live_frames": len(live), "fixture_frames": len(fixtures),
                      "exact_fixture_frames_in_live": matches,
                      "matching_bytes": sum(row["bytes"] for row in matches)}, indent=2))
