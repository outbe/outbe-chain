# ADR-B-SUP-001: Supervision, failure taxonomy, readiness and observability

- **Status:** Proposed; projection supervision is strong but lifecycle coverage is incomplete
- **Date:** 2026-07-17
- **Decision owners:** Blockchain Space, node lifecycle and SRE/operations maintainers
- **Scope:** process supervision, health state, failure classification, metrics/logs/traces and operator probes
- **Depends on:** ADR-B-NOD-001, ADR-B-GEN-001, ADR-B-OCD-004 through ADR-B-OCD-015
- **Related:** ADR-B-TXP-001, ADR-B-OCD-011 and every ADR that declares fatal or retryable failures

## Context

An Outbe process owns Reth execution/networking, Commonware consensus/marshal/DKG,
CE persistence, Mongo projection, RPC, TEE connectivity and several background
actors. A task can stop, panic, lose its writer lease, stall or become inconsistent
while the OS process and HTTP socket remain alive. Logs and counters cannot repair
that ambiguity: operators need one authoritative lifecycle state and exact reasons.

The code already contains useful pieces: projection has structured sticky status and
reports fatal exit to the top-level lifecycle; execution has a watchdog; consensus
exports detailed counters/gauges; RPC exposes projection health; startup validates
several mandatory dependencies. These pieces do not yet form one node-wide
supervision and readiness contract.

This ADR owns local operational truth. It never changes consensus validity or uses a
wall-clock health timeout as a block-validity rule.

## Decision

### One node supervisor and component registry

The root lifecycle owns a `NodeSupervisor` with one registered record for every
long-lived task, thread, external dependency and durable writer. Each record declares:

- stable component/instance ID and role applicability;
- mandatory, conditional or optional criticality;
- startup dependencies and shutdown order;
- task/thread handle, cancellation and bounded join contract;
- heartbeat/progress/checkpoint semantics, not merely “task exists”;
- permitted recovery/restart policy and retry budget;
- failure classifier and redacted diagnostic fields; and
- readiness contributions and operator runbook reference.

Mandatory work cannot be spawned detached. Unexpected normal return, panic, closed
mailbox/readiness publisher, writer-lease loss or exhausted retry budget produces a
terminal supervisor event. The first terminal cause is sticky and initiates common
cancellation exactly once; later failures are attached as shutdown diagnostics.

### Node lifecycle state machine

The externally visible node state is:

```text
Starting
  -> Recovering(checkpoints)
  -> CatchingUp(targets)
  -> Ready(role, exact finalized/canonical/projection identities)
  -> Degraded(reason, bounded recovery)
  -> Draining(cause)
  -> Stopped(result)

Any state -> Fatal(first_cause) -> Draining -> Stopped(nonzero)
```

Transitions are monotonic for one process generation except bounded
`Ready <-> Degraded` transitions explicitly allowed for local dependencies. `Fatal`
is sticky. A new process generation receives a unique boot ID so stale probes and
metrics cannot be confused with recovery.

### Failure taxonomy

Every failure is classified along independent axes:

| Axis        | Values                                                                                                               |
| ----------- | -------------------------------------------------------------------------------------------------------------------- |
| Origin      | configuration, compatibility, consensus, execution, persistence, projection, dependency, resource, operator shutdown |
| Determinism | deterministic data/invariant vs local/transient availability                                                         |
| Severity    | request-rejected, degraded, not-ready, fatal-process, integrity-emergency                                            |
| Retry       | none, bounded immediate, bounded backoff, restart/import/operator action                                             |
| Scope       | request, actor, node, validator role, network evidence                                                               |

Unknown errors default to fatal for mandatory consensus/state writers, not transient.
String matching is not classification authority. Typed errors carry a stable code,
component, safe message, causal chain, relevant exact checkpoint/height/hash and
recommended action. Error codes are append-only API values.

Consensus-invalid input is distinct from local inability to verify it. Corruption,
same-height hash/root conflict and impossible state never become retry loops. Network,
Mongo or TEE unavailability may degrade/retry only within a finite policy and only
when no unsafe participation/read is possible.

### Liveness, readiness and role gates

Probes have separate meanings:

- **live:** supervisor/event loop can respond and is not irrecoverably dead;
- **startup:** initialization/recovery is still legitimately progressing within its
  declared deadline;
- **ready:** this role may receive traffic/participate without violating an ADR;
- **health detail:** authenticated diagnostic document with component states.

`ready` for a validator requires exact chain/profile identity, ADR-B-OCD-014 recovery
convergence, Reth canonical/finalized health, CE marker, Mongo projection boundary
required by execution, consensus/marshal/DKG/committee readiness, active signing
material, TEE readiness when the chain requires it, writer leases and resource
headroom. A follower has no signing-key requirement but must have certified ancestry
and the same execution/projection gates. RPC readiness declares which
consistency tiers are serviceable.

Ready is computed from typed component snapshots in one generation; it is never a
manually set boolean or inferred solely from peer count/block height. Exact heights
also carry hashes/roots where equality matters. Any missing mandatory publisher is
fatal or not-ready according to its declared lifecycle phase.

### Progress and watchdogs

A heartbeat is useful only with an expected progress relation. Watchdogs compare:

- consensus finalized tip, marshal delivered/acknowledged height and Reth canonical/
  finalized hash;
- CE finalized marker and Mongo projection checkpoint;
- actor queue depth/bytes/oldest age from ADR-B-OCD-009;
- current epoch/DKG activation windows and signing material expiry;
- writer lease/fencing epoch;
- TEE attestation/session/key epoch; and
- disk, MDBX/Reth map, memory/file-descriptor and external dependency budgets.

Thresholds are role/profile configuration with startup grace and hysteresis. A local
watchdog can stop participation or the node; it never declares a block invalid.
System time rollback cannot make an unhealthy duration negative or immortal.

### Metrics, structured logs and traces

Every terminal/degraded transition emits the same stable failure code to:

- a sticky supervisor snapshot and exit status;
- one structured log with boot/component IDs and safe context;
- a low-cardinality counter/gauge and alert state; and
- a trace/event correlation where enabled.

Metrics have documented type, unit, labels, cardinality bound and reset semantics.
Addresses, hashes, heights, peer IDs, error strings and request IDs are not labels.
They may appear in sampled/redacted structured events. Gauges expose current state;
counters are monotonic per boot; histograms use reviewed bounded buckets. Duplicate
log storms are rate-limited while a counter records suppressed occurrences.

Secrets, private keys/shares, tokens, Mongo/upstream credentials, TEE plaintext and
raw user payloads never enter logs, metrics, panic messages or health responses.
URLs are sanitized before `Debug`/error formatting. Diagnostic endpoints require an
operator exposure/auth policy and return versioned, bounded documents.

### Shutdown and exit contract

Signals, operator requests and fatal components converge on one cancellation token.
Shutdown stops admission/proposal first, drains or rejects in-flight work, preserves
consensus acknowledgements/durable writer ordering, releases leases and joins every
registered task in dependency order. Each join has a bounded deadline and records
timeout/panic; the process never waits forever or silently detaches a mandatory task.

Clean operator shutdown exits zero. Startup/configuration/fatal runtime failures exit
nonzero with a stable top-level code. The supervisor persists or prints a final
redacted component report after joins, preserving the first cause.

### Operational evidence

CI injects return, error, panic, hang, closed channel, lease loss, corrupt checkpoint,
dependency outage/recovery and shutdown at every mandatory component. Tests assert
state transitions, readiness removal, cancellation, join order, exit code, sticky
first cause and emitted metrics without relying on log text. Deployment probes and
alerts are exercised against a real local network.

## Authoritative interfaces

| Responsibility                  | Authority                                              |
| ------------------------------- | ------------------------------------------------------ |
| Component ownership/criticality | `NodeSupervisor` registry                              |
| Current lifecycle/readiness     | versioned supervisor snapshot                          |
| Failure semantics               | stable typed failure-code registry                     |
| External probes                 | role-aware liveness/readiness/health RPC/HTTP contract |
| Metrics and labels              | checked observability schema                           |
| Shutdown and exit result        | root supervisor first-cause report                     |

## Invariants

- Every mandatory long-lived task/thread/dependency has one root owner and join path.
- Unexpected mandatory component exit cannot leave the node reporting ready.
- The first fatal cause is sticky and produces nonzero process exit.
- Readiness is role-specific and based on exact consistent checkpoints/identities.
- Local timeout/health state never changes consensus validity.
- Retry is typed, bounded and cannot hide deterministic corruption.
- Metrics labels have finite cardinality and contain no secrets/user identifiers.
- Shutdown is dependency-ordered, bounded and preserves durable acknowledgement
  barriers.
- Health/probe responses are bounded, versioned and truthful for one boot generation.

## Atomicity, replay and failure

Supervisor transition plus readiness publication is one serialized operation: once a
fatal event is accepted, no concurrent healthy update can restore readiness. Events
carry component generation/fencing IDs so late work from a replaced component is
ignored. Cancellation is idempotent.

Observability is not state-transition authority and is not replayed into consensus.
Metric/log exporter failure is itself observable through local fallback and may
degrade operations, but cannot block consensus threads. Persistent diagnostic files
are local, bounded, redacted and never imported as chain state.

## Compatibility and migration

Failure codes, lifecycle states, probe JSON and metric names are versioned operator
interfaces. Fields may be added compatibly; meaning, units or label sets do not
change in place. Deprecated metrics overlap for a declared release window. Role and
readiness requirement changes are release/activation reviewed because they alter
safe deployment behavior even if consensus bytes do not change.

## Production-interface verification evidence

Inspected top-level node launch/select/join paths, projection supervisor and sticky
readiness, consensus executor/finalization actors, execution watchdog, Commonware
telemetry, consensus metrics, RPC projection/consensus status and TEE/Mongo startup
fail-fast paths. Projection correctly converts deterministic failures, unexpected
ExEx return and publisher loss into structured fatal status and top-level shutdown.
The process does not yet expose a single role-aware supervisor snapshot or join every
critical component through one live failure select. Status remains Proposed.

## Consequences

“Process is running” stops being confused with “validator is safe and ready”. Every
module audit can declare how its failure affects node state, operators get stable
alerts/runbooks, and background task death cannot remain hidden behind a healthy RPC
socket.

## Rejected alternatives

- **Use logs as supervision:** logs are lossy observations, not lifecycle authority.
- **Treat every error as retryable:** corruption and invariant failure can create an
  indefinitely unsafe zombie node.
- **One `/health` boolean:** liveness, startup, readiness and diagnostic health answer
  different operational questions.
- **Panic/abort on every actor error:** it loses structured shutdown/durable ordering
  and first-cause evidence.
- **High-cardinality metrics for convenience:** hashes/addresses/errors make the
  monitoring system itself a resource failure.

## Open questions and technical debt

1. **Critical:** the top-level async `select` watches node exit, projection exit and
   Ctrl-C, but the consensus `std::thread` result is joined only after shutdown has
   already begun. Route early consensus return/error/panic into immediate supervisor
   shutdown and readiness removal.
2. **Critical:** no single `NodeSupervisor` registry proves ownership and terminal
   propagation for every mandatory Reth, consensus, marshal, DKG, CE, Mongo, TEE and
   RPC task/thread.
3. **Critical:** health/readiness is fragmented among projection watch state,
   ancestry readiness, bridge consensus status, execution watchdog and RPC methods.
   Build one role-aware snapshot with exact identities and sticky fatal cause.
4. Audit every raw `spawn`, detached OS thread, Commonware actor and blocking worker.
   Register mandatory handles or prove they are scoped request work with bounded
   lifetime and cancellation.
5. Projection detached blocking work may outlive its async manager after cancellation.
   Prove late completion cannot write/checkpoint under a lost lease and bound thread/
   shutdown accumulation.
6. Projection has a strong typed `ProjectionFailureClass`; generalize the pattern
   without collapsing component-specific evidence into free-form strings.
7. `PrecompileError::Fatal(String)` remains an unstructured catch-all used across
   consensus execution. Introduce stable codes and preserve safe structured causes.
8. Distinguish transient local body/tree/projection unavailability from deterministic
   corrupt canonical data through payload build, validation, RPC and supervisor. No
   path may convert “could not verify” into “invalid block”.
9. Execution watchdog has grace logic and tests; connect its fatal decision to the
   root supervisor/readiness/exit code and expose exact observed checkpoints.
10. Define watchdog behavior under system-clock rollback. Prefer monotonic instants
    within a boot and explicit unknown duration after persisted/restart boundaries.
11. Prove consensus/finalization mailbox closure outside graceful shutdown becomes a
    fatal supervisor event. Several paths log/count a dropped message because closure
    may be normal during shutdown; phase/generation must disambiguate it.
12. Add count/byte/age metrics and alerts for every ADR-B-OCD-009 actor queue, especially
    currently unbounded executor/finalization/peer-manager channels.
13. Define validator readiness for DKG preparing/degraded/expired randomness states,
    pending activation and missing shares; current `ConsensusStatus::is_active` is
    useful but not the complete node gate.
14. Define TEE readiness across socket reachability, attestation freshness/policy,
    registration, sealed offer key and key epoch. A connected process is insufficient.
15. Mongo startup uses a fixed eight-second total deadline. Make operational deadline
    policy explicit by role/profile and classify timeout without turning wall-clock
    speed into consensus validity.
16. RPC exposes projection health, but add complete liveness/startup/readiness/detail
    contracts and bind server traffic admission to them. Avoid returning HTTP success
    with an undocumented unhealthy body.
17. Audit `outbe-cli monitor health` semantics against the new probe contract and make
    its exit codes automation-safe for ready/degraded/not-ready/fatal states.
18. Inventory every metric name/type/unit/label and enforce it in CI. Existing
    consensus metrics are extensive, but there is no project-wide checked schema.
19. Remove dynamic strings, hashes, addresses, epochs/views and peer IDs from metric
    labels wherever present; validate cardinality under long-running/adversarial use.
20. Add metrics for supervisor state, component generation, restart/retry budget,
    last progress age, exact checkpoint gaps, lease status and shutdown join results.
21. Audit logs/errors/Debug output for private keys, DKG shares, upstream/Mongo URL
    credentials, bearer tokens, TEE material and user encrypted payloads. Add redaction
    property tests across all argument paths.
22. Define bounded graceful-shutdown deadlines and escalation for Reth, consensus,
    marshal ACKs, CE/Mongo writers, enclave and exporter. Current joins can otherwise
    delay termination indefinitely.
23. Preserve the first fatal cause when secondary actors fail due to cancellation;
    current error wrapping/thread join order can report a downstream symptom instead.
24. Add fault-injection lifecycle tests for every component return/error/panic/hang,
    dependency outage/recovery, signal race and simultaneous fatal causes.
25. Test deployment probes and alert rules against a real validator/follower under
    stalled finality, projection lag/ahead/conflict, CE mismatch, lost TEE, disk-full,
    queue overload and consensus-thread death.
26. Publish a runbook mapping each stable failure/readiness code to automatic action,
    evidence preservation and ADR-B-OCD-006, ADR-B-OCD-014 and ADR-B-OCD-015 repair paths.
