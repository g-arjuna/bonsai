# ADR — Bonsai-native network intelligence, not a required SuzieQ runtime

**Date:** 11 November 2026  
**Status:** Accepted direction; implementation and external support remain unverified  
**Decision scope:** architecture and phased backlog only; no collector, parser, schema, UI, or security behavior changes in this ADR  
**Code baseline:** Bonsai `main` at `d5833fe82ac7a510599d6bc010d21b6ad6e9aef2`; SuzieQ OSS `develop` at `75198b5585ebef91f0659b6b1ec3c964cc2cd1ad`  
**Owner:** BN-024; integration gates remain under BN-008/013/015/016/019/021/022/023

## Decision

Build a **Bonsai-native Rust network-state normalization and configuration-intelligence capability** inside the existing Bonsai product, with no mandatory SuzieQ installation or SuzieQ storage/poller service. Study and, where technically and legally appropriate, selectively adapt **SuzieQ OSS service mappings, vendor normalizer algorithms, schemas and fixtures**. Do **not** automatically translate the entire Python implementation or assume its configuration collector provides a complete configuration-semantics compiler.

The architectural objective is one coherent product and evidence model for **intended, configured, effectively configured, observed and independently validated** network facts. Default standalone deployment should need no separate SuzieQ application. A single Rust runtime is the preferred destination for eligible trusted parsers and read-only collectors, but **one product does not require one process**: isolate potentially unsafe plugins/inputs where needed; retain existing Python adapters during demonstrated parity migration. Distributed ingress and routing workers remain valid under BN-023.

SuzieQ is an **engineering reference and optional comparator**, not the authoritative inventory, runtime dependency, graph database or explanation engine. Existing PyATS/Genie/TextFSM and BMP/gNMI/routing functionality remain until validated replacements; deleting a sidecar because a Rust module compiles is prohibited.

## Why now

Bonsai collects extensive operational evidence, while its native configuration understanding is comparatively shallow:

- `src/config_store.rs` securely stores encrypted configuration snapshots, and `src/change_detection.rs` tracks snapshot hashes and text diffs, but this does not prove semantics of peer groups, routing policies, route-target imports or forwarding dependencies.
- `src/enrichment/parser_chain_enricher.rs` uses CLI capture and `src/parser_chain.rs` dispatches to PyATS/Genie and a nominal `suzieq_native` backend. That nominal backend routes to `docker/sidecars/bonsai-native-parser/app.py`, whose parser is presently line extraction, **not SuzieQ's normalizer**.
- `src/graph/queries.rs::shortest_topology_path` traverses physical-ish graph connectivity; it is **not route-, VRF-, policy-, or time-aware forwarding verification**. `/api/path` must not be advertised as dataplane path proof.
- `src/telemetry_identity.rs` already distinguishes transport, exporter and trusted device identity. A new CLI/config source must obey that contract instead of using hostname/IP as unrestricted authority.
- The previously approved `docs/architecture/adr_suzieq_vs_pyats.md` **deferred SuzieQ as the primary collector**. This ADR adds a new option—selective native Rust reuse—without reversing that historically appropriate conclusion.

SuzieQ OSS contains service YAML, normalized schemas, Python vendor-specific cleaners, JSON template extraction, and path analysis in `suzieq/engines/pandas/path.py`. Its `devconfig` module primarily obtains raw config and uses CiscoConfParse for section extraction; it is **not** a complete multi-NOS effective-configuration parser. The new semantic engine is separate design work.

## Decisions and non-negotiable boundaries

1. **Collect once where practical, interpret many times.** Prefer existing validated gNMI/NETCONF/CLI/BMP data and safe snapshot reuse; choose an additional poll only for a demonstrated coverage gap. Capture planning is NOS-version/feature/transport-specific and inventory-first, never gNMI-mandatory.
2. **Versioned, typed fact boundary.** Every output identifies canonical scope (environment/tenant/device/VRF/network instance, SONiC namespace/ASIC where relevant), phenomenon, layer, source + authentication status, raw evidence reference, parser/catalog version, validity interval, observation/ingestion time, coverage and errors. No inferred authoritative device or physical link from an untrusted hostname/IP.
3. **Five truth planes stay distinct.** Intended (NetBox/Nautobot/manual/CMDB authority), configured (device statements), effective (resolved inheritance/default/policies on supported NOS), observed (live control/operational evidence) and validated (independent dataplane/assurance result). A graph assertion never silently promotes between them.
4. **Configuration source is sensitive.** Encrypted raw snapshots remain restricted in `ConfigStore`, with explicit sanitation, encryption-at-rest and access policy. The graph and LLM receive only scoped semantic facts and authorized redacted excerpts. No secrets, arbitrary CLI, unbounded graph queries or device writes through analysis.
5. **Fail partial, not falsely complete.** Unsupported statements, unpolled devices, parse ambiguity, unknown policy semantics, collection errors, missing gNMI delete, stale BMP view and replay gaps return typed `unknown / unsupported / incomplete / conflicting`. Missing evidence does not mean absent or healthy.
6. **Graph stores relations, not every byte.** Keep bulky configs, RIBs/FIBs, counters and event histories in suitable encrypted/indexed stores. Graph projected entities and edges link back to immutable raw evidence and store/model versions; as-of/diff must be coherent across stores.
7. **Rust port via differential fixtures.** Pin the upstream OSS revision; inventory file-level copyrights/licenses; implement a restricted declarative extractor (not Python `eval`), explicit vendor cleaners, conformance fixtures, fuzzing and shadow comparison. No blind line-for-line translation, undocumented clean-room claim or automatic acceptance of published sample coverage as field proof.
8. **SONiC is a first-class future extension case.** Support its version/distribution/HWSKU/ASIC and DB namespace differences. CONFIG_DB defines configured intent, APPL_DB/STATE_DB represent intermediate application/state observations, and ASIC_DB exposure is **not** itself proof of physical ASIC packet forwarding. Favor supported read-only management APIs over direct privileged Redis access.
9. **One operator experience and one curated AI evidence boundary.** No mandatory SuzieQ/CMDB, no second product UI. Explain paths with separate topology, predicted and observed labels, per-hop evidence and termination reasons. LLM tools deliver scoped deterministic assessments with counterevidence/coverage and explicit no-data states.
10. **Ship by verified capability, not marketing.** Report per NOS/version/feature as decoded (D), ingested (I), normalized (N), semantically resolved (S), graph-projected (P), independently validated (V) and operationally proven (O). A green parser test alone proves only the applicable level.

## Trade-offs and alternatives

| Option | Outcome |
|---|---|
| Require SuzieQ OSS deployment and read adapter | Lower initial porting cost, but adds another scheduler, database, credential plane and runtime. Retain only as a **benchmark/fallback experiment**, not the target architecture. |
| Copy/translate all SuzieQ Python | High upkeep and accidental Pandas/poller coupling; port its behavior selectively behind Bonsai contracts instead. |
| Keep PyATS/TextFSM indefinitely, never add semantics | Valid bridge, but cannot satisfy typed configuration-to-topology and path explanations alone. Retain until native parity. |
| Build every parser/forwarding solver from scratch | Reinvents tested vendor details; reject. Reuse upstream OSS knowledge, YANG/OpenConfig, available vendor APIs and independent tools as reference oracles. |
| Embed an exhaustive symbolic network digital twin immediately | Too broad and false-confidence prone. Start with small supported semantic families and independently checked paths; Batfish-style comparison may remain a non-runtime test oracle. |

## Supersession and related owners

This ADR **supplements** `docs/architecture/adr_suzieq_vs_pyats.md` (SuzieQ-as-poller remains deferred) and refines the exploratory `BN-015-U7` SuzieQ read-adapter benchmark: evaluate **native normalization parity and long-term ownership** as the primary decision, with an optional read adapter solely to compare evidence where it helps. It **does not overwrite** existing BN-003/006/007/008/013/015/016/019/021/022/023 acceptance.

Execution lives exclusively in `BONSAI_BACKLOG.md` under **BN-024** (new distinct semantic-engine workstream), cross-referencing the existing owning slices. Detailed source seams, data contracts, phased acceptance and SONiC extension rules are in [Native Network Intelligence design](NATIVE_NETWORK_INTELLIGENCE_DESIGN_2026-11-11.md).

## Explicit deferrals

No claim of full multi-vendor semantic coverage, arbitrary vendor command support, network-wide symbolic reachability, forwarding/ASIC equivalence, ML/LLM correctness, live configuration writes, complete parity with SuzieQ paths/assertions, or production SONiC support is made by this documentation change. Each requires its named live/negative/replay gates.

## Upstream/source references

- SuzieQ OSS: https://github.com/netenglabs/suzieq/tree/75198b5585ebef91f0659b6b1ec3c964cc2cd1ad and Apache-2.0 LICENSE; verify individual imported files and included third-party assets separately.
- SuzieQ normalization: `suzieq/config/*.yml`, `config/schema/*.avsc`, `poller/worker/services/{service,svcparser,interfaces,bgp,routes}.py`.
- SuzieQ `devconfig`: `engines/pandas/devconfig.py`; path engine: `engines/pandas/path.py`.
- SONiC telemetry/DB layers: https://github.com/sonic-net/SONiC/blob/master/doc/system-telemetry/grpc_telemetry.md.
- SONiC multi-namespace/ASIC: https://github.com/sonic-net/SONiC/blob/master/doc/database/multi_namespace_db_instances.md.
- Historical Bonsai foundation: [Config-state lane](config_state_lane.md), [UI/onboarding trust review](UI_ONBOARDING_TOPOLOGY_PRODUCT_REVIEW_2026-11-11.md).
