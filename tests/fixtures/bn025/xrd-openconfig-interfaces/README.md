# XRd OpenConfig interface fixture

`openconfig-interface-root-subscribe-xrd-24.4.2.json` is the unmodified stdout of a
read-only gNMI `subscribe-once` request for
`/openconfig-interfaces:interfaces/interface[name=*]` against the dedicated
authorized lab container `clab-bn025-xrd-xrd` (`ios-xr/xrd-control-plane:24.4.2`,
management address 172.100.106.21, plaintext lab-only gNMI port 9339) on
2026-11-25 UTC. Credentials were supplied through environment variables and are
not present in the fixture. The raw capture and capability evidence are retained
under `docs/merger/evidence/2026-11-25-bn025-xrd-native-interfaces/`.

This is genuine versioned XRd evidence for offline semantic normalization. It is
not by itself production live-acquisition qualification.
