# ADR governance and evidence contract

- **Status:** Implemented
- **Date:** 2026-07-17
- **Scope:** Entire repository; documentation and review process only
- **Depends on:** Root `README.md`
- **Supersedes:** No project-wide ADR policy existed

## Context

Outbe Chain combines Reth execution, Commonware Simplex consensus, stateful Rust
precompiles, TEE execution, asynchronous Mongo projection, authenticated local
state, operator tooling, and multiple economic state machines in one workspace.
The repository already contains a deep ADR-001–013/017 series, but that series is
specific to off-chain entity storage. It cannot by itself explain authority,
atomicity, ordering, and recovery across the rest of the node.

Architecture review of a stateful module needs stable answers for module scope, mutation
entrypoints, persistent invariants, FSM transitions, atomicity domains, receipts,
replay semantics, deterministic bounds, migration rules, and production-interface
tests. Scattered comments or a passing test suite cannot supply that contract.

## Decision

### Three Architecture Spaces and stable identities

Every current ADR belongs to exactly one responsibility space and is stored under
its matching directory:

- `ADR-B-MMM-NNN` in `docs/adr/blockchain/ADR-B-MMM-NNN-<slug>.md` owns the replicated network/execution
  substrate;
- `ADR-S-MMM-NNN` in `docs/adr/system/ADR-S-MMM-NNN-<slug>.md` owns network-wide operating and evolution
  mechanisms; and
- `ADR-C-MMM-NNN` in `docs/adr/core/ADR-C-MMM-NNN-<slug>.md` owns Consume-to-Gain business protocol state.

The space prefix is carried by the directory, filename and full document heading/ID;
the filename always begins with the complete `ADR-<space>-<module>-NNN` identity. Bare numeric
filenames such as `081-<slug>.md` are forbidden. Thus the complete identity
`ADR-B-OCD-007` is stored as `docs/adr/blockchain/ADR-B-OCD-007-<slug>.md`.

The space follows architectural responsibility, not an accidental crate path. The
three-letter module code is a stable owner registered in [`index.md`](index.md), not
necessarily a crate. Sequence is dense and top-down inside each `(space, module)`
pair: a module with `N` entries uses exactly `001..N`. During reconstruction a module
may be atomically renumbered; after acceptance new decisions append that module's
next number. A cross-space outcome is a PFS, never an aggregate ADR family.

The canonical definitions of Blockchain, System and Core are maintained in
[`CONTEXT.md`](../../CONTEXT.md), and the ordered catalog is
[`index.md`](index.md).

### ADR granularity follows state ownership

A stateful ADR has exactly one primary module/state owner and one public mutation
boundary. Cross-module workflows are documented as imported seams and dependency
links, not by merging several independently auditable modules into one record.
Ledger and factory packages receive separate ADRs when either owns persistent
state, authorization, replay guards, indexes, or an independently meaningful
architecture review. A combined ADR is allowed only when inspection proves the parts
form one aggregate with no independently callable mutation boundary; that proof
must be stated explicitly.

Project-wide concerns such as execution ordering, activation, cryptography and
verification may be cross-cutting ADRs, but they must not absorb the FSM or debt of
the modules they coordinate.

`docs/adr/index.md` is the canonical project-wide ADR catalog and coverage ledger.
Every architectural decision that affects a caller-visible invariant, persistent
schema, consensus result, trust boundary, ordering rule, atomicity domain,
activation rule, or externally visible failure mode must be recorded by one ADR.

The root README remains the top-level normative product contract. The precedence
order is:

1. explicit network/fork specification, once one exists;
2. root README product contract;
3. accepted/implemented ADRs, with newer explicit supersession winning;
4. module README and public interface documentation;
5. code and production-interface tests as implementation evidence;
6. comments, implementation logs, plans, and historical proposals.

Lower layers cannot silently redefine higher layers. A disagreement is recorded
as technical debt or resolved by deliberately changing the authoritative source.

## ADR interface

An ADR is a decision record, not a design wishlist or audit report. It states the
chosen architecture and enough observable invariants to let callers, implementers,
and auditors distinguish conforming behavior from accidental behavior.

Each stateful ADR exposes this review interface:

```text
scope + authority + command
    -> guard(current state, provenance, time/height)
    -> deterministic transition plan
    -> owned atomicity domain and typed effects
    -> committed outcome | retryable error | fatal invariant error
```

It must name any route that bypasses that shape: raw facade construction,
macro-generated dispatch, callbacks, registries, trait defaults, re-exports,
test-only adapters, and operator repair tools are part of the effective interface.

## Architectural evidence profile

For every stateful module or module family, its owning ADR must provide or link to
current evidence for all ten architecture review gates:

| Gate | ADR evidence required |
|---|---|
| G1 Deep, closed interface | Production commands/queries, callers, authority and bypass inventory |
| G2 Valid state model | Persisted tags, decoding, record/index and cross-field invariants |
| G3 Explicit FSM | Complete state/event/guard/effect/next-or-error table |
| G4 Atomicity | Real checkpoint/transaction owner for every effect path |
| G5 Effects/receipts | Named effects, typed consumed receipts, propagated failures |
| G6 Determinism/bounds | Ordering, caps, cursors, gas/work and starvation policy |
| G7 Single-source invariants | One owner for duplicated records, indexes, counters and cross-module facts |
| G8 Replay/concurrency | Duplicate intent, retry, terminal replay, reentrancy and linearization semantics |
| G9 Production evidence | Tests through the real interface and transaction model |
| G10 Project contract | README/ABI/schema/fork/operator migration impact |

An ADR does not assign the implementation a safety verdict. Reviews remain
evidence-based; the ADR makes the intended contract and known gaps inspectable.

## Evidence and status changes

- `Proposed -> Accepted` requires deliberate human architectural approval.
- `Accepted -> Implemented` requires inspected production-path evidence and the
  verification commands/results recorded in the ADR or a linked immutable report.
- Passing unit tests alone never changes status.
- Supersession names the replacing ADR and identifies which decisions remain
  historical context only.
- Open questions are not silently deleted when resolved; they are converted to a
  dated resolution, linked ADR, or verified closure entry.

## Determinism and atomicity

This ADR changes documentation only and has no consensus-visible execution or
persistent state. Its own atomicity domain is one reviewed repository change.
Normative code changes described by another ADR retain that ADR's activation and
rollback policy.

## Verification

The policy is observable when:

- `docs/adr/index.md` exists and enumerates every workspace architecture area;
- every indexed ADR has one status and one owning scope;
- every completed ADR contains an open-questions/debt section;
- links resolve and no decision has two editable canonical documents;
- a module auditor can map every discovered mutation entrypoint to an owning ADR
  or report it as an uncovered architecture defect.

The final project-wide completion audit must compare `cargo metadata` workspace
packages, registered precompiles/lifecycle hooks, node construction, RPC methods,
binaries, scripts and test harnesses against the index rather than relying on a
hand-maintained crate list alone.

## Consequences

- Architecture debt becomes visible instead of being inferred from comments.
- ADR work is larger: stateful decisions must specify failure and recovery, not
  just a happy-path data model.
- module audits can distinguish “decision missing” from “implementation violates
  the decision”.
- The ADR catalog itself becomes maintained architecture and requires review when
  new packages, precompiles, persistent stores or externally asynchronous effects
  are introduced.

## Rejected alternatives

### Treat code as the only source of truth

Rejected because reachable behavior cannot reveal which ordering, fallback or
failure mode is intentional. It also makes architectural defects indistinguishable
from undocumented decisions.

### One monolithic architecture document

Rejected because it cannot own detailed FSM and atomicity facts for dozens of
modules without becoming unreviewable and stale. The index supplies system shape;
focused ADRs own decisions at real seams.

### Let passing tests imply architectural safety

Rejected by the evidence contract: tests are evidence only after confirming that
they cross the production interface and cover reachable mutation/effect paths.

## Open questions and technical debt

- No repository-enforced ADR link/status/schema linter exists yet.
- Human approval and ownership rules are not defined; `Implemented` in this ADR
  means the documentation mechanism is present, not that every decision is approved.
- Existing `/adr` records must be reconciled without creating two normative copies.
- The repository has no immutable verification-report convention; command output
  currently lives in prose and `impl_log.md`.
- Decide whether accepted module-review reports should link from ADRs or from a
  separate audit index.
- There is not yet an automated inventory of macro-generated precompile dispatch,
  lifecycle registration, and raw storage-facade construction for coverage drift.
- Prefix/path/header agreement and complete `ADR-` references are currently checked
  manually; promote the structural checks used during the B/S/C migration into CI.
