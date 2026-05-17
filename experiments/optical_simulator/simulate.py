#!/usr/bin/env python3
"""Optical channel telemetry simulator (D2-7 T4).

Emits synthetic OpenConfig-conforming optical channel state events to stdout
and optionally to a bonsai HTTP endpoint. Used to validate the D2-7 data model
and optical_rx_degrading detection rule before real hardware is available.

Usage:
    python3 simulate.py                              # steady state, stdout only
    python3 simulate.py --scenario degrade           # rx_power drops 0.5 dBm/tick
    python3 simulate.py --bonsai-url http://...      # POST to bonsai
    python3 simulate.py --list-scenarios             # print available scenarios
"""
from __future__ import annotations

import argparse
import json
import math
import random
import sys
import time
import urllib.error
import urllib.request
from dataclasses import asdict, dataclass

# ── Channel defaults (OpenConfig optical-channel/state) ─────────────────────

CHANNELS = ["ch-1", "ch-2", "ch-3", "ch-4"]

_RX_NOMINAL = -3.0      # dBm — healthy receive power
_RX_FAIL_THRESHOLD = -12.0  # dBm — optical_rx_degrading fires below this
_TX_NOMINAL = 2.0       # dBm
_OSNR_NOMINAL = 30.0    # dB
_PRE_FEC_NOMINAL = 1e-4
_BIAS_NOMINAL = 52.0    # mA
_TEMP_NOMINAL = 44.0    # °C


@dataclass
class ChannelState:
    name: str
    rx_power_dbm: float = _RX_NOMINAL
    tx_power_dbm: float = _TX_NOMINAL
    osnr_db: float = _OSNR_NOMINAL
    pre_fec_ber: float = _PRE_FEC_NOMINAL
    laser_bias_ma: float = _BIAS_NOMINAL
    temperature_c: float = _TEMP_NOMINAL

    def add_noise(self, sigma: float = 0.05) -> None:
        self.rx_power_dbm += random.gauss(0, sigma)
        self.tx_power_dbm += random.gauss(0, sigma * 0.5)
        self.osnr_db += random.gauss(0, sigma * 2)
        self.pre_fec_ber *= (1 + random.gauss(0, 0.05))
        self.pre_fec_ber = max(1e-9, self.pre_fec_ber)
        self.temperature_c += random.gauss(0, 0.1)


# ── Scenario definitions ─────────────────────────────────────────────────────

SCENARIOS: dict[str, str] = {
    "steady":       "All channels healthy, rx_power held near nominal −3 dBm.",
    "degrade":      "One channel (ch-1) rx_power drops 0.5 dBm/tick over 20 ticks, crossing −12 dBm.",
    "hard_fail":    "Channel ch-1 drops to −40 dBm in one step — sudden fibre cut.",
    "flap":         "Channel ch-1 rx_power oscillates ±4 dBm every 30s — connector vibration.",
    "multi_degrade":"All channels degrade simultaneously — rack-level fibre event.",
}


def _apply_scenario(
    scenario: str,
    channels: dict[str, ChannelState],
    tick: int,
    target: str,
) -> None:
    ch = channels.get(target)
    if scenario == "steady":
        pass  # noise applied after

    elif scenario == "degrade":
        if ch and tick <= 20:
            ch.rx_power_dbm = _RX_NOMINAL - tick * 0.5
            # OSNR degrades in proportion to rx loss
            ch.osnr_db = _OSNR_NOMINAL - tick * 0.3
            ch.pre_fec_ber = _PRE_FEC_NOMINAL * (10 ** (tick * 0.1))

    elif scenario == "hard_fail":
        if ch and tick == 1:
            ch.rx_power_dbm = -40.0
            ch.osnr_db = 0.0
            ch.pre_fec_ber = 1.0

    elif scenario == "flap":
        if ch:
            period = 30
            ch.rx_power_dbm = _RX_NOMINAL + 4.0 * math.sin(2 * math.pi * tick / period)

    elif scenario == "multi_degrade":
        for c in channels.values():
            if tick <= 20:
                c.rx_power_dbm = _RX_NOMINAL - tick * 0.5
                c.osnr_db = _OSNR_NOMINAL - tick * 0.3


def _emit(
    device_address: str,
    channels: dict[str, ChannelState],
    bonsai_url: str | None,
) -> None:
    occurred_at_ns = int(time.time() * 1e9)
    payload = {
        "device_address": device_address,
        "event_type": "optical_channel_state",
        "occurred_at_ns": occurred_at_ns,
        "channels": [asdict(c) for c in channels.values()],
    }
    line = json.dumps(payload)
    print(line, flush=True)

    if bonsai_url:
        url = f"{bonsai_url.rstrip('/')}/api/events/inject"
        data = line.encode("utf-8")
        req = urllib.request.Request(
            url,
            data=data,
            method="POST",
            headers={"Content-Type": "application/json"},
        )
        try:
            with urllib.request.urlopen(req, timeout=5):
                pass
        except urllib.error.HTTPError as exc:
            print(f"[simulator] HTTP {exc.code} on inject: {exc.read().decode()}", file=sys.stderr)
        except Exception as exc:
            print(f"[simulator] inject failed: {exc}", file=sys.stderr)


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Bonsai optical channel telemetry simulator (D2-7 T4)"
    )
    parser.add_argument("--scenario", default="steady", choices=list(SCENARIOS),
                        help="Fault scenario to simulate (default: steady)")
    parser.add_argument("--channel", default="ch-1",
                        help="Target channel for single-channel scenarios (default: ch-1)")
    parser.add_argument("--device", default="optical-line-01.dc1",
                        help="Simulated device address")
    parser.add_argument("--ticks", type=int, default=60,
                        help="Number of ticks to emit (default: 60; 0 = infinite)")
    parser.add_argument("--interval", type=float, default=1.0,
                        help="Seconds between ticks (default: 1.0)")
    parser.add_argument("--bonsai-url", default=None,
                        help="POST events to this bonsai base URL (e.g. http://127.0.0.1:3000)")
    parser.add_argument("--list-scenarios", action="store_true",
                        help="List available scenarios and exit")
    args = parser.parse_args()

    if args.list_scenarios:
        print("Available scenarios:")
        for name, desc in SCENARIOS.items():
            print(f"  {name:16s}  {desc}")
        return

    channels: dict[str, ChannelState] = {ch: ChannelState(name=ch) for ch in CHANNELS}

    tick = 0
    try:
        while args.ticks == 0 or tick < args.ticks:
            tick += 1
            _apply_scenario(args.scenario, channels, tick, args.channel)
            for ch in channels.values():
                ch.add_noise()
            _emit(args.device, channels, args.bonsai_url)
            time.sleep(args.interval)
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
