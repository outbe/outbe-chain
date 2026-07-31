# ADR-B-NOD-001: One process owns execution, consensus/following, projection, and shutdown

- **Status:** Proposed (documents the observed current implementation)
- **Date:** 2026-07-17
- **Scope:** `bin/outbe-chain`, `crates/blockchain/node`, lifecycle wiring in `crates/blockchain/engine`
- **Depends on:** ADR governance, ADR-B-OCD-005 and ADR-B-OCD-008 through ADR-B-OCD-013
- **Related:** ADR-B-GEN-001 genesis/chain identity, ADR-B-CNS-003 consensus/execution bridge, ADR-B-SUP-001 readiness/fatality

## Context

Outbe Chain deliberately has no HTTP Engine API split. One binary embeds Reth and
Commonware, shares an in-process `ConsensusExecutionBridge`, installs mandatory
off-chain projection and compressed-entity services, and exposes one Reth RPC
surface. This reduces distributed coordination but creates one large lifecycle
authority: startup order, mode selection, health gates, fatal propagation, and
shutdown must be unambiguous or one subsystem can continue after another has lost
the state required for safe execution.

The current production root is `run_node` in `bin/outbe-chain/src/main.rs:272`.
It constructs every long-lived subsystem directly; there is no runtime plugin
registry for protocol-critical components.

## Decision

The `outbe-chain` process is the lifecycle owner for:

- Reth execution, networking, payload building and RPC;
- validator consensus or certified follower synchronization;
- finalized Mongo body projection and its readiness state;
- compressed-entity MDBX, exact-parent execution reads, finalized commit and
  startup recovery;
- optional TEE sidecar connection/attestation;
- binary protocol-version compatibility.

The node starts in exactly one operational mode:

| Mode | Selector | Consensus/follow runtime | Signing authority | RPC authority |
|---|---|---|---|---|
| Validator | `--validator` | Full Commonware consensus | BLS share plus required EVM signer | Full bridge-backed consensus status/finalization |
| Certified follower | `--upstream` without validator | Lightweight certified follow stack | None | May serve chained finalization, must not report validator status |
| Plain execution full node | neither | None | None | Disabled in the ADR-B-OCD-005 execution profile |

`validate_adr005_node_mode` rejects the third mode
(`bin/outbe-chain/src/main.rs:207-213`) because historical execution currently
requires a certified finalized-parent projection barrier. The branch in
`run_node:653-725` nevertheless remains implementation code and is treated as a
dormant effective interface, not proof that the mode is supported.

## Startup transition

```text
Parsed
  -> cryptographic globals initialized
  -> CLI/config validated
  -> required storage/pruning constraints validated
  -> optional TEE connected and attested
  -> validator EVM signer loaded (validator only)
  -> Mongo projection topology prepared
  -> CE MDBX identity opened and speculative candidates discarded
  -> Reth node + ExEx + RPC launched
  -> CE durable finalizer/recovery adapters constructed
  -> active protocol version checked against binary
  -> consensus/follower runtime spawned and handed the live node
  -> Running(mode)
```

Any synchronous error before `Running` is fatal startup failure. There is no
degraded startup that silently removes Mongo, CE, required signer, configured
TEE, or protocol-version checks.

### Ordering evidence

- ZK CRS initialization precedes the Tokio runtime because its blocking setup is
  not async-safe (`main.rs:277-282`).
- Projection connection/topology is prepared before Reth component launch
  (`main.rs:490-503`); its canonical checkpoint is validated inside ExEx startup
  (`main.rs:568-579`).
- CE environment identity binds local schema, chain ID, genesis hash, commitment
  scheme, topology and vendor revision before opening MDBX (`main.rs:507-545`).
- Binary compatibility is checked after the provider exists and before the
  consensus/follower thread is spawned (`main.rs:639-663`).
- Validator EVM signing key is mandatory in validator mode
  (`main.rs:470-489`).

These orderings are part of the interface and may not be rearranged as incidental
initialization cleanup without updating this ADR and its failure tests.

## Running and shutdown FSM

| Current | Event | Guard | Effects | Next/error |
|---|---|---|---|---|
| Running validator/follower | Reth exit | none | cancel consensus token; join consensus thread | Stopped or fatal join error |
| Running validator/follower | consensus/follower exit | none | leave select; cancel shared token; join thread | Stopped or propagated task error |
| Running any supported mode | projection fatal/exit | structured `ProjectionExit` received | request Reth engine shutdown where available | Stopped |
| Running any supported mode | Ctrl-C | signal received | cancel consensus token in consensus/follower modes | Stopped |
| Running full-node branch | Reth exit/Ctrl-C/projection fatal | branch is currently unreachable through mode validation | stop/select behavior | Stopped |
| Consensus thread | cancellation token | runtime active | return `Ok(())` | Joined |
| Consensus thread | stack error | runtime active | log and propagate error | Fatal |
| Consensus thread | panic | join observes unwind | resume unwind in process owner | Fatal panic |

`handle_consensus_thread_join` preserves the distinction among clean completion,
returned error and panic (`main.rs:88-93`). Projection publishes a typed failure
class and fatal readiness before notifying the process owner
(`node/src/projection.rs:967-984`).

## Authority and effective interface

The external seam is the CLI plus chain spec. Internal constructors such as
`OutbeNode::with_bridge`, `OutbeNode::with_bridge_and_evm_signer`, projection
preparation/checkpoint validation, and CE adapter traits are not independent
operator interfaces; `run_node` owns their production ordering.

Tests or alternate binaries that construct `OutbeNode` directly are part of the
effective interface for architecture review gates G1/G9. They must not be used as evidence for safe
startup unless they reproduce the production gates relevant to the behavior under
test.

## State and single-source invariants

The lifecycle owner does not persist a separate “node state” record. It derives
mode and readiness from authoritative subsystem state:

- CLI mode flags select validator/follower behavior;
- Reth provider owns canonical execution/finality data;
- Mongo owns its durable projection checkpoint and identity;
- CE MDBX owns its environment identity and finalized marker;
- on-chain Update state owns active protocol version;
- consensus key/share files live below effective consensus/key directories.

Required cross-store consistency is not proven by this ADR; ADR-B-OCD-007 owns restart
reconciliation. Startup must fail closed when an implemented validator detects a
conflict. It must not repair one authority from another without a named recovery
policy and durable receipt.

## Side-effect ledger

| Effect | Owner | Atomicity domain | Receipt/error | Retry/recovery |
|---|---|---|---|---|
| Open/validate Mongo projection | projection preparation | Mongo topology/identity checks | propagated `Result` | startup retry by operator |
| Open/validate CE MDBX | `CeMdbx::open` | local MDBX environment | propagated `Result` | ADR-B-OCD-014 recovery only |
| Connect/attest configured TEE | TributeFactory client initialization | external sidecar handshake | propagated `Result` | startup retry; no silent fallback for configured socket |
| Spawn Reth | Reth builder | Reth-owned lifecycle | `NodeHandle` or error | process restart |
| Spawn consensus/follower | process owner + Commonware runner | separate OS thread/runtime | join result | whole-process restart |
| Projection fatal notification | projection supervisor | watch status + lifecycle channel | `ProjectionExit` | whole-node shutdown/restart |
| Slashing/governance JSONL journals | primitives journal initializers | local append-only diagnostic files | warning only | best effort; explicitly non-authoritative |
| Metrics/logs | each subsystem | diagnostic/metering | non-transactional | may survive failed operation |

The JSONL journals are diagnostic, not protocol state: initialization failure is
logged and startup continues (`main.rs:351-372`). No recovery or business logic may
treat their absence as proof that an on-chain transition did not happen.

## Determinism and concurrency

Consensus and Reth execute in separate runtimes/threads and communicate through
owned bridges/channels. Projection work may run on detached blocking threads. The
architecture therefore is not globally serialized; each cross-runtime operation
needs its own linearization/acknowledgement rule in the owning ADR.

Mode selection and startup validation are process-local and deterministic for a
fixed CLI, chain spec, local durable stores and sidecar responses. Wall-clock
timeouts affect local availability/participation only and must not choose a
consensus-visible state transition.

## Security and trust assumptions

- The process and local filesystem are trusted as the validator host; the TEE host
  relay is not trusted with offer plaintext/key authority (ADR-S-TEE-001).
- Mongo and CE MDBX are required local materializations, not independent consensus
  authorities.
- A certified upstream is trusted only through verified finalization/committee
  chaining, not because of its network identity (ADR-B-GEN-001).
- Diagnostic journals and metrics are untrusted for protocol recovery.

## Verification evidence

Current evidence includes:

- node projection unit tests for typed fatal publication and recovery behavior;
- `crates/blockchain/node/tests/projection_startup.rs` startup validation cases;
- Rust e2e follower, restart, lifecycle and projection scenarios, run through the
  production binary;
- the full harness result recorded on 2026-07-17: 12 scenarios and 83 steps passed.

This evidence is partial for the lifecycle as a whole. There is no single
production-interface fault matrix injecting failure at every startup and shutdown
boundary.

## Consequences

- The process has one place to enforce fail-closed construction and coordinate
  shutdown.
- Protocol-critical adapters cannot be independently hot-swapped at runtime.
- A fatal local dependency can intentionally remove the entire node from service.
- The large `run_node` implementation concentrates authority but currently also
  concentrates substantial construction complexity in one function.

## Rejected alternatives

### HTTP Engine API split

Rejected for the current architecture: it adds a remotely authenticated protocol
and another failure/atomicity seam without a deployment requirement.

### Let each subsystem terminate independently

Rejected because execution could remain externally available after losing a
mandatory projection or consensus/follow authority.

### Automatically fall back from validator to follower/full-node mode

Rejected because it silently changes signing and RPC authority. Mode changes
require explicit operator intent and successful startup validation.

## Open questions and technical debt

- **Decision required:** `main.rs:443-447` says that omitting the TEE socket uses
  an in-process TEE stub, while the root README says offer decryption has no
  in-process key path. This normative/code-comment conflict must be resolved in
  ADR-S-TEE-001 and ADR-C-TRB-002 and verified against current TributeFactory construction.
- The plain full-node branch remains after `validate_adr005_node_mode` makes it
  unreachable. Either define its future activation guard in ADR-B-OCD-007 and ADR-B-OCD-008 or delete
  the dormant behavior to close the effective interface.
- Projection fatal shutdown explicitly calls Reth shutdown, but consensus exit and
  Ctrl-C rely on surrounding handle/drop behavior before cancellation/join. A
  production-interface shutdown test must prove no Reth task or port survives all
  exit causes.
- `ProjectionExit` delivery uses an unbounded channel and ignores send failure.
  The lifecycle safety argument currently depends on the receiver living as long
  as the node select loop; structural ownership should make that invariant explicit.
- Consensus/follower task completion is signalled by a `oneshot<()>` that loses the
  returned error until thread join. Verify that every select path always joins and
  propagates the original error without deadlock.
- Diagnostic journal initialization is best effort, but disk-full behavior during
  later appends and log integrity/rotation are not documented.
- CE candidate limits are `usize::MAX` pending ADR-B-OCD-009; local memory exhaustion can
  become a node-availability failure.
- Startup creates several trait-object adapters after Reth launch. Failure in later
  compatibility or consensus startup leaves cleanup to handle/drop semantics; add
  deterministic fault injection at each post-launch boundary.
- No top-level `NodeMode` enum makes mutually exclusive modes unrepresentable;
  validity currently depends on boolean CLI combinations and validation order.
- This ADR requires human acceptance. Its status must not be promoted merely
  because it accurately describes current code.
