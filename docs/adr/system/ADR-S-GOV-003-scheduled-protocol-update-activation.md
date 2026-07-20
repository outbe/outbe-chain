# ADR-S-GOV-003: Update owns protocol-version scheduling and activation

- **Status:** Proposed; current implementation profiled
- **Date:** 2026-07-17
- **Owners/scope:** `crates/system/update`, EVM upgrade-handler registry and node
  startup compatibility gate
- **Depends on:** ADR-B-CNS-003, ADR-B-EVM-004, ADR-S-GOV-002
- **Supersedes:** The Update-local portions of the deleted pre-space governance aggregate

## Context

An approved executable vote is authorization to schedule an update, not proof that
the running binary can activate it. Scheduling, migration execution, active-version
state and startup compatibility form one separate consensus boundary.

## Decision

Update is the sole owner of scheduled-update records, the waiting index, active
protocol version/height and version history. Only the registered Update vote target
may create a schedule from an approved Vote payload. At activation height,
compile-time upgrade handlers run atomically before the active version changes.
A version with no handler is an intentional version-only activation.

## Input and scheduling invariants

The JSON payload carries a nonzero encoded version, activation height and info. The
version must exceed the active version at scheduling time; the height must satisfy
the chain-specific minimum buffer; no other waiting update may use that height; a
proposal id may schedule once; and the waiting list is capped at 64. Localnet uses
a zero activation buffer, while other chains use the compiled 100-block buffer.

State consists of active version, its activation height, a height-to-version history,
scheduled records keyed by Vote proposal id, and a dense waiting-id list. Every
waiting id must resolve to one `Scheduled` record. At most one scheduled record may
occupy an activation height.

## State machine

```text
Scheduled --height reached, version > active, supported--> Activated
Scheduled --version <= active----------------------------> Canceled
```

`Activated` and `Canceled` are terminal. After activation, all still-waiting updates
whose version is less than or equal to the new active version are canceled. Multiple
future versions may therefore coexist, but activation order is determined by block
height and stale lower/equal versions cannot roll the protocol back.

## Ordering, atomicity and failure

Vote finalization runs before Update activation in the same begin-block sequence, so
localnet's zero buffer permits a newly approved schedule to activate in that block
when its declared height is already reached. Each activation runs in a checkpoint:
all registered handlers for the version run first, then active version/history,
record status, waiting indexes, events and stale cancellations commit together.

A handler failure is promoted to fatal and rolls the checkpoint back. Activating a
version above the running binary's compiled protocol version is fatal on every
chain, deliberately refusing to produce a block under unsupported rules. Replayed
begin-block sees a terminal record and performs no second migration.

## Compatibility and trust boundary

Every chain uses the binary's compiled protocol version as its activation ceiling;
devnet/testnet have no compatibility bypass. Node startup accepts active version
zero or any version not newer than its binary and refuses an older binary.
Upgrade-handler order is compile-time list order, and all validators must ship the
same handlers. Missing handlers only warn because version-only activation is valid.

Protocol version encoding, payload schema, handler order and activation constants
are consensus compatibility surfaces. They may change only through an activation
rule understood by both pre- and post-upgrade binaries.

## Production-interface evidence

Evidence inspected in `crates/system/update/src/{vote_target,payload,runtime,state,
handlers,startup,constants}.rs`, Vote target dispatch, EVM handler wiring and
activation/rollback/replay tests. Structural closure requires restart tests against
persisted active state, multi-handler order tests and mixed-binary network tests.

## Consequences and rejected alternatives

This design makes migrations deterministic and keeps proposal mechanics outside
Update storage. Direct public scheduling was rejected because it bypasses Vote.
Changing active version before migrations was rejected because observers could see
new rules with old state. Requiring a handler for every version was rejected to
permit rule/feature activations with no state migration.

## Open questions and technical debt

- Define whether same-block Vote approval and activation is intended on localnet;
  zero buffer plus begin-block ordering currently permits it.
- Reconcile the devnet/testnet activation ceiling with the startup binary-version
  gate and document which test binaries may safely activate versions they do not
  advertise.
- Define deterministic ordering when several different activation heights are
  already overdue after restart; the waiting list uses swap-remove order.
- Prove every production binary has identical handler membership and order, and
  reject duplicate handler identities if order-dependent effects are unsafe.
- Define cancellation authority for a future update before activation; no public
  governance cancellation path is visible.
- Specify rollback/recovery when an accepted upgrade handler can never succeed.
- Add mixed-version network, crash-at-checkpoint and version-history closure tests.
