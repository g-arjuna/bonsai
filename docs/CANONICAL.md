# Bonsai repository orientation

Updated: 13 October 2026.

Start with [current state](STATUS.md) and [the common Bonsai backlog](../BONSAI_BACKLOG.md).
These are the current state and work authorities. The [claim validation](merger/CLAIM_VALIDATION_2026-10-13.md)
records actual checks and pending acceptance. `AGENTS.md` and current user
instructions govern development and authorization.

Bonsai owns the public UI, inventory, configuration and authentication.
RustyBMP's protocol, RIB and history components live in `crates/rbmp-core`,
`crates/rbmp-rib` and `crates/rbmp-store`. Routing service state is exposed through
Bonsai's proxy and `/routing` UI and projected into the graph. Python sidecars
provide rules and experimental ML. This is an integrated development prototype;
operational autonomous execution and predictive model quality are not accepted
capabilities merely because their helper code exists.

## Where to start

| Purpose | Reference |
|---|---|
| One active work queue and historical-ID crosswalk | [BONSAI_BACKLOG.md](../BONSAI_BACKLOG.md) |
| Current verified capabilities and first actions | [STATUS.md](STATUS.md) |
| Claim audit, commands and fresh evidence | [13 October validation](merger/CLAIM_VALIDATION_2026-10-13.md) |
| Development contract | [AGENTS.md](../AGENTS.md) |
| Routing integration design | [RustyBMP handoff](../BONSAI_RUSTYBMP_CODEX_HANDOFF.md) |
| Investigation/ML/assurance design | [Agentic NetOps strategy](../BONSAI_AGENTIC_NETOPS_STRATEGY_2026.md) |
| Architecture decisions | [DECISIONS.md](../DECISIONS.md) |
| Sidecar architecture | [sidecars.md](architecture/sidecars.md) |
| Adapter cursor and cold-start contract | [adapter_cursor_cold_start.md](architecture/adapter_cursor_cold_start.md) |
| Existing feature/test catalog; reconcile dates with current review | [FEATURE_INDEX.md](testing/FEATURE_INDEX.md) |
| Earlier planning records | [backlog archive](backlog_archive/README.md) |
| Previous canonical reference, preserved unchanged | [historical orientation](backlog_archive/2026-10-13/docs/CANONICAL.md) |

## Working from here

Inspect the actual checkout and local changes. Pick one ready BN item and its
acceptance criteria, read the relevant current source and run focused checks.
Record evidence and remaining limits in the existing backlog and state snapshot.
Do not create another sprint backlog or infer completion from historical tables.

The former canonical document contained May-era environment restrictions,
gNMI-only scope, an empty DV2 sprint pointer, unavailable CV7/graphify/memory
links and automatic pull/push instructions. It is now an archived reference.
Current user instructions and the development contract govern which actions are
authorized. Historical lab, migration and operational procedures are references,
not instructions to execute unchanged.
