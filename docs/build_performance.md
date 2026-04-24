
# Bonsai Build Performance Baseline

This document tracks build times and binary/image sizes to monitor the impact of Tier 1 build optimisations.

## Baseline (v5 Sprint 1)

Captured on 2026-04-23 using `scripts/build_bench.sh` (manually executed due to timeouts).
Environment: Ubuntu (native).

| Task | Duration | Notes |
|---|---|---|
| Clean Rust build | 23:00 | Cold cache, all dependencies |
| Incremental Rust build | 0:20 | Source change in src/main.rs |
| Cargo check | 0:41 | --release |
| Cargo test | 0:54 | --release, 34 tests passed |
| UI build | 0:01 | npm ci && npm run build |
| Docker build (clean) | 40:28 | --no-cache |
| Docker build (cache hit) | 0:04 | BuildKit cache hit on dependencies |

**Release Binary Size**: 27 MB
**Docker Image Size**: 131 MB

## Tier 1 Optimisations

### sccache Integration (Sprint 2)

`sccache` has been integrated via `.cargo/config.toml`. It caches Rust compilation artifacts across clean builds.

**Installation**:
Run `scripts/install_sccache.sh` or `cargo install sccache`.

**Expected Impact**:
40-70% faster clean builds after the first one.

### Docker Parallel Stage Verification (Sprint 2)

**Status**: ✅ Verified.

BuildKit logs confirm that the `ui-builder` stage and the `chef`/`rust-builder` stages execute in parallel. The UI build starts immediately and finishes long before the Rust build, ensuring it does not gate the total build time.

## Tier 3 Optimisations (T3-1, Sprint 4)

### cargo-chef planner: manifest-only COPY

Previously the `rust-planner` stage used `COPY . .`, causing the cargo-chef
cook step (all external dependencies, ~35 min) to re-run whenever *any* file
changed — including Svelte sources, docs, proto files, and shell scripts.

**Fix**: the planner stage now copies only `Cargo.toml` and `Cargo.lock`.
The cook step re-runs only when dependency versions change. Source-only edits
result in a cache hit on the cook layer and only the final `cargo build` step
runs (~4s incremental).

**Expected impact on Docker build time**:

| Scenario | Before | After (estimated) |
|---|---|---|
| Clean build (cold cache) | 40 min | ~40 min (unchanged — first build always full) |
| Source-only change | ~40 min (cook re-ran) | ~5 min (only final build step) |
| `Cargo.lock` change | ~40 min | ~40 min (cook must re-run) |

### curl replaced with compiled healthcheck binary

`curl` has been removed from the runtime image. A stdlib-only Rust binary
(`src/bin/healthcheck.rs`, 337 KB stripped) makes a raw TCP probe to
`/api/readiness`. This also fixes a latent bug: `docker-compose.yml` used
`/usr/local/bin/healthcheck` as the healthcheck command, but the image only
contained `curl`.

**Estimated size savings**: ~4–5 MB (curl + libcurl shared library).

### liblbug.so.0 stripped with --strip-debug

The C++ graph DB shared library now has debug symbols removed while retaining
the symbol table required for dynamic linking. This reduces the library's
on-disk size in the image.

## Analysis

The clean build times (23m for Rust, 40m for Docker) are excessive and confirm that Build Optimisation (Tier 1) is a high-priority requirement. The incremental build time (20s) is acceptable but likely to creep up as the codebase grows.
---
### Dependency Audit: 2026-04-23 08:32:28 UTC

#### Duplicate Dependencies
```
base64 v0.21.7
├── age v0.11.2
│   └── bonsai v0.1.0 (/home/arjuna/Desktop/bonsai)
└── age-core v0.11.0
    └── age v0.11.2 (*)

base64 v0.22.1
├── arrow-cast v54.3.1
│   └── parquet v54.3.1
│       └── bonsai v0.1.0 (/home/arjuna/Desktop/bonsai)
├── metrics-exporter-prometheus v0.16.2
│   └── bonsai v0.1.0 (/home/arjuna/Desktop/bonsai)
├── parquet v54.3.1 (*)
└── tonic v0.13.1
    └── bonsai v0.1.0 (/home/arjuna/Desktop/bonsai)

bitflags v1.3.2
└── flatbuffers v24.12.23
    └── arrow-ipc v54.3.1
        └── parquet v54.3.1 (*)

bitflags v2.11.1
├── raw-cpuid v11.6.0
│   └── quanta v0.12.6
│       ├── metrics-exporter-prometheus v0.16.2 (*)
│       └── metrics-util v0.19.1
│           └── metrics-exporter-prometheus v0.16.2 (*)
├── rustix v1.1.4
│   └── tempfile v3.27.0
│       └── prost-build v0.13.5
│           └── tonic-build v0.13.1
│               [build-dependencies]
│               └── bonsai v0.1.0 (/home/arjuna/Desktop/bonsai)
│       [dev-dependencies]
│       └── bonsai v0.1.0 (/home/arjuna/Desktop/bonsai)
└── tower-http v0.6.8
    └── bonsai v0.1.0 (/home/arjuna/Desktop/bonsai)

bytes v1.11.1
└── prost v0.13.5
    └── prost-build v0.13.5 (*)

bytes v1.11.1
├── arrow-buffer v54.3.1
│   ├── arrow-array v54.3.1
│   │   ├── arrow-cast v54.3.1 (*)
│   │   ├── arrow-ipc v54.3.1 (*)
│   │   ├── arrow-select v54.3.1
│   │   │   ├── arrow-cast v54.3.1 (*)
│   │   │   └── parquet v54.3.1 (*)
│   │   ├── bonsai v0.1.0 (/home/arjuna/Desktop/bonsai)
│   │   └── parquet v54.3.1 (*)
│   ├── arrow-cast v54.3.1 (*)
│   ├── arrow-data v54.3.1
│   │   ├── arrow-array v54.3.1 (*)
│   │   ├── arrow-cast v54.3.1 (*)
│   │   ├── arrow-ipc v54.3.1 (*)
│   │   ├── arrow-select v54.3.1 (*)
│   │   └── parquet v54.3.1 (*)
│   ├── arrow-ipc v54.3.1 (*)
│   ├── arrow-select v54.3.1 (*)
│   └── parquet v54.3.1 (*)
├── axum v0.8.9
│   ├── bonsai v0.1.0 (/home/arjuna/Desktop/bonsai)
│   └── tonic v0.13.1 (*)
├── axum-core v0.5.6
│   └── axum v0.8.9 (*)
├── h2 v0.4.13
│   ├── hyper v1.9.0
│   │   ├── axum v0.8.9 (*)
│   │   ├── hyper-rustls v0.27.9
│   │   │   └── metrics-exporter-prometheus v0.16.2 (*)
│   │   ├── hyper-timeout v0.5.2
│   │   │   └── tonic v0.13.1 (*)
│   │   ├── hyper-util v0.1.20
│   │   │   ├── axum v0.8.9 (*)
│   │   │   ├── hyper-rustls v0.27.9 (*)
│   │   │   ├── hyper-timeout v0.5.2 (*)
│   │   │   ├── metrics-exporter-prometheus v0.16.2 (*)
│   │   │   └── tonic v0.13.1 (*)
│   │   ├── metrics-exporter-prometheus v0.16.2 (*)
│   │   └── tonic v0.13.1 (*)
│   └── tonic v0.13.1 (*)
├── http v1.4.0
│   ├── axum v0.8.9 (*)
│   ├── axum-core v0.5.6 (*)
│   ├── h2 v0.4.13 (*)
│   ├── http-body v1.0.1
│   │   ├── axum v0.8.9 (*)
│   │   ├── axum-core v0.5.6 (*)
│   │   ├── http-body-util v0.1.3
│   │   │   ├── axum v0.8.9 (*)
│   │   │   ├── axum-core v0.5.6 (*)
│   │   │   ├── metrics-exporter-prometheus v0.16.2 (*)
│   │   │   ├── tonic v0.13.1 (*)
│   │   │   └── tower-http v0.6.8 (*)
│   │   ├── hyper v1.9.0 (*)
│   │   ├── hyper-util v0.1.20 (*)
│   │   ├── tonic v0.13.1 (*)
│   │   └── tower-http v0.6.8 (*)
│   ├── http-body-util v0.1.3 (*)
│   ├── hyper v1.9.0 (*)
│   ├── hyper-rustls v0.27.9 (*)
│   ├── hyper-util v0.1.20 (*)
│   ├── tonic v0.13.1 (*)
│   └── tower-http v0.6.8 (*)
├── http-body v1.0.1 (*)
├── http-body-util v0.1.3 (*)
├── hyper v1.9.0 (*)
├── hyper-util v0.1.20 (*)
├── parquet v54.3.1 (*)
├── prost v0.13.5
│   ├── bonsai v0.1.0 (/home/arjuna/Desktop/bonsai)
│   ├── prost-types v0.13.5
│   │   ├── bonsai v0.1.0 (/home/arjuna/Desktop/bonsai)
│   │   ├── prost-build v0.13.5 (*)
│   │   └── tonic-build v0.13.1 (*)
│   └── tonic v0.13.1 (*)
├── tokio v1.52.1
│   ├── axum v0.8.9 (*)
│   ├── bonsai v0.1.0 (/home/arjuna/Desktop/bonsai)
│   ├── h2 v0.4.13 (*)
│   ├── hyper v1.9.0 (*)
│   ├── hyper-rustls v0.27.9 (*)
│   ├── hyper-timeout v0.5.2 (*)
│   ├── hyper-util v0.1.20 (*)
│   ├── metrics-exporter-prometheus v0.16.2 (*)
│   ├── tokio-rustls v0.26.4
│   │   ├── hyper-rustls v0.27.9 (*)
│   │   └── tonic v0.13.1 (*)
│   ├── tokio-stream v0.1.18
│   │   ├── bonsai v0.1.0 (/home/arjuna/Desktop/bonsai)
│   │   └── tonic v0.13.1 (*)
│   ├── tokio-util v0.7.18
│   │   ├── h2 v0.4.13 (*)
│   │   ├── tokio-stream v0.1.18 (*)
│   │   ├── tower v0.5.3
│   │   │   ├── axum v0.8.9 (*)
│   │   │   └── tonic v0.13.1 (*)
│   │   └── tower-http v0.6.8 (*)
│   ├── tonic v0.13.1 (*)
│   ├── tower v0.5.3 (*)
│   └── tower-http v0.6.8 (*)
├── tokio-util v0.7.18 (*)
├── tonic v0.13.1 (*)
└── tower-http v0.6.8 (*)

getrandom v0.2.17
├── rand_core v0.6.4
│   ├── rand v0.8.6
│   │   ├── age v0.11.2 (*)
│   │   └── age-core v0.11.0 (*)
│   ├── rand_chacha v0.3.1
│   │   └── rand v0.8.6 (*)
│   └── x25519-dalek v2.0.1
│       └── age v0.11.2 (*)
└── ring v0.17.14
    ├── rustls v0.23.38
    │   ├── bonsai v0.1.0 (/home/arjuna/Desktop/bonsai)
    │   ├── hyper-rustls v0.27.9 (*)
    │   └── tokio-rustls v0.26.4 (*)
    └── rustls-webpki v0.103.12
        └── rustls v0.23.38 (*)

getrandom v0.3.4
├── ahash v0.8.12
│   ├── arrow-array v54.3.1 (*)
│   ├── arrow-select v54.3.1 (*)
│   ├── metrics v0.24.3
│   │   ├── bonsai v0.1.0 (/home/arjuna/Desktop/bonsai)
│   │   ├── metrics-exporter-prometheus v0.16.2 (*)
│   │   └── metrics-util v0.19.1 (*)
│   └── parquet v54.3.1 (*)
└── rand_core v0.9.5
    ├── rand v0.9.4
    │   └── metrics-util v0.19.1 (*)
    ├── rand_chacha v0.9.0
    │   └── rand v0.9.4 (*)
    └── rand_xoshiro v0.7.0
        └── metrics-util v0.19.1 (*)

getrandom v0.4.2
├── tempfile v3.27.0 (*)
└── uuid v1.23.1
    ├── bonsai v0.1.0 (/home/arjuna/Desktop/bonsai)
    └── lbug v0.15.3
        └── bonsai v0.1.0 (/home/arjuna/Desktop/bonsai)

hashbrown v0.15.5
├── arrow-array v54.3.1 (*)
├── metrics-util v0.19.1 (*)
└── parquet v54.3.1 (*)

hashbrown v0.17.0
└── indexmap v2.14.0
    ├── h2 v0.4.13 (*)
    ├── metrics-exporter-prometheus v0.16.2 (*)
    ├── petgraph v0.7.1
    │   └── prost-build v0.13.5 (*)
    ├── serde_yaml v0.9.34+deprecated
    │   └── bonsai v0.1.0 (/home/arjuna/Desktop/bonsai)
    ├── toml_edit v0.22.27
    │   └── toml v0.8.23
    │       └── bonsai v0.1.0 (/home/arjuna/Desktop/bonsai)
    └── tower v0.5.3 (*)

i18n-embed v0.15.4
└── i18n-embed-fl v0.9.4 (proc-macro)
    └── age v0.11.2 (*)

i18n-embed v0.15.4
└── age v0.11.2 (*)

log v0.4.29
├── i18n-config v0.4.8
│   ├── i18n-embed-fl v0.9.4 (proc-macro) (*)
│   └── i18n-embed-impl v0.8.4 (proc-macro)
│       ├── i18n-embed v0.15.4 (*)
│       └── i18n-embed v0.15.4 (*)
├── i18n-embed v0.15.4 (*)
└── prost-build v0.13.5 (*)

log v0.4.29
├── i18n-embed v0.15.4 (*)
├── rustls v0.23.38 (*)
├── tracing v0.1.44
│   ├── axum v0.8.9 (*)
│   ├── axum-core v0.5.6 (*)
│   ├── bonsai v0.1.0 (/home/arjuna/Desktop/bonsai)
│   ├── h2 v0.4.13 (*)
│   ├── hyper-util v0.1.20 (*)
│   ├── metrics-exporter-prometheus v0.16.2 (*)
│   ├── tonic v0.13.1 (*)
│   ├── tower v0.5.3 (*)
│   ├── tower-http v0.6.8 (*)
│   └── tracing-subscriber v0.3.23
│       └── bonsai v0.1.0 (/home/arjuna/Desktop/bonsai)
└── tracing-log v0.2.0
    └── tracing-subscriber v0.3.23 (*)

prost v0.13.5 (*)

prost v0.13.5 (*)

rand v0.8.6 (*)

rand v0.9.4 (*)

rand_chacha v0.3.1 (*)

rand_chacha v0.9.0 (*)

rand_core v0.6.4 (*)

rand_core v0.9.5 (*)

regex-automata v0.4.14
├── matchers v0.2.0
│   └── tracing-subscriber v0.3.23 (*)
└── tracing-subscriber v0.3.23 (*)

regex-automata v0.4.14
└── regex v1.12.3
    └── prost-build v0.13.5 (*)

regex-syntax v0.8.10
└── regex-automata v0.4.14 (*)

regex-syntax v0.8.10
├── regex v1.12.3 (*)
└── regex-automata v0.4.14 (*)

rustc-hash v1.1.0
└── fluent-bundle v0.15.3
    └── fluent v0.16.1
        ├── i18n-embed v0.15.4 (*)
        ├── i18n-embed v0.15.4 (*)
        └── i18n-embed-fl v0.9.4 (proc-macro) (*)

rustc-hash v2.1.2
└── type-map v0.5.1
    └── intl-memoizer v0.5.3
        ├── fluent-bundle v0.15.3 (*)
        ├── i18n-embed v0.15.4 (*)
        └── i18n-embed v0.15.4 (*)

self_cell v0.10.3
└── fluent-bundle v0.15.3 (*)

self_cell v1.2.2
└── self_cell v0.10.3 (*)

serde_core v1.0.228
├── axum v0.8.9 (*)
├── serde_json v1.0.149
│   ├── axum v0.8.9 (*)
│   └── bonsai v0.1.0 (/home/arjuna/Desktop/bonsai)
└── serde_path_to_error v0.1.20
    └── axum v0.8.9 (*)

serde_core v1.0.228
└── serde v1.0.228
    ├── basic-toml v0.1.10
    │   └── i18n-config v0.4.8 (*)
    ├── bonsai v0.1.0 (/home/arjuna/Desktop/bonsai)
    ├── i18n-config v0.4.8 (*)
    ├── rmp-serde v1.3.1
    │   └── bonsai v0.1.0 (/home/arjuna/Desktop/bonsai)
    ├── serde_spanned v0.6.9
    │   ├── toml v0.8.23 (*)
    │   └── toml_edit v0.22.27 (*)
    ├── serde_urlencoded v0.7.1
    │   └── axum v0.8.9 (*)
    ├── serde_yaml v0.9.34+deprecated (*)
    ├── toml v0.5.11
    │   └── find-crate v0.6.3
    │       ├── i18n-embed-fl v0.9.4 (proc-macro) (*)
    │       └── i18n-embed-impl v0.8.4 (proc-macro) (*)
    ├── toml v0.8.23 (*)
    ├── toml_datetime v0.6.11
    │   ├── toml v0.8.23 (*)
    │   └── toml_edit v0.22.27 (*)
    ├── toml_edit v0.22.27 (*)
    └── unic-langid-impl v0.9.6
        └── unic-langid v0.9.6
            ├── i18n-config v0.4.8 (*)
            ├── i18n-embed v0.15.4 (*)
            └── i18n-embed-fl v0.9.4 (proc-macro) (*)

socket2 v0.5.10
└── tonic v0.13.1 (*)

socket2 v0.6.3
├── hyper-util v0.1.20 (*)
└── tokio v1.52.1 (*)

toml v0.5.11 (*)

toml v0.8.23 (*)

twox-hash v1.6.3
└── parquet v54.3.1 (*)

twox-hash v2.1.2
└── lz4_flex v0.11.6
    └── parquet v54.3.1 (*)

unic-langid v0.9.6
├── fluent v0.16.1 (*)
├── fluent-bundle v0.15.3 (*)
├── fluent-langneg v0.13.1
│   ├── fluent-bundle v0.15.3 (*)
│   ├── i18n-embed v0.15.4 (*)
│   └── i18n-embed v0.15.4 (*)
├── i18n-embed v0.15.4 (*)
├── intl-memoizer v0.5.3 (*)
└── intl_pluralrules v7.0.2
    └── fluent-bundle v0.15.3 (*)

unic-langid v0.9.6 (*)

unic-langid-impl v0.9.6
└── unic-langid v0.9.6 (*)

unic-langid-impl v0.9.6 (*)
```


<!-- CI_BASELINE_SECONDS=0 last_updated=2026-04-24 -->
