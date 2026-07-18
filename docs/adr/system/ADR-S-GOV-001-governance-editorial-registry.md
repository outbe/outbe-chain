# ADR-S-GOV-001: Governance owns the editorial proposal registry

- **Status:** Proposed; current implementation profiled
- **Date:** 2026-07-17
- **Owners/scope:** `crates/core/governance`; OIP/GIP text, editorial status,
  authorities, canon and meta-canon revisions
- **Depends on:** ADR-B-EVM-004
- **Supersedes:** The module-local portions of the deleted pre-space governance aggregate

## Context

The Governance precompile records human-readable protocol proposals and canonical
documents. It does not execute protocol changes. Conflating this editorial record
with validator voting or binary activation obscures three independent authorities
and makes illegal cross-module states difficult to audit.

## Decision

Governance is the sole owner of OIP/GIP editorial records, their text hashes and
status indexes, the governance authority set, and canon/meta-canon revision
history. Executable approval belongs to Vote (ADR-S-GOV-002), while protocol version
activation belongs to Update (ADR-S-GOV-003). A Governance `Approved` or `Implemented`
status is documentary evidence only and grants no execution capability.

## Authoritative interface and authority

Any address may submit an OIP or GIP in `Draft`. Only the recorded author may edit
text while a proposal is `Draft` or `Rework`; the supplied hash must equal the
proposal bytes and text is capped at 128 KiB. Authorities may change status,
replace canon/meta-canon, and add another authority. The one author exception is
`Rework -> Draft`, used to resubmit edited text.

The inspected authority model is additive: genesis seeds the initial authorities
and no removal command is visible. No Governance method may schedule an Update or
dispatch arbitrary module payloads.

## Persistent state, indexes and invariants

OIP and GIP use separate monotonically allocated ids and separate records, author
lists, and status-enumeration sets. Every existing proposal must occur exactly once
in its author's dense list and exactly once in the set for its current status.
Stored `text_hash` must equal the stored text. Missing records use the schema's
sentinel representation and must never be returned as allocated ids.

Canon and meta-canon each own current bytes, version, hash, and a revision-hash
history. A replacement increments its version and appends the new hash. Historical
text itself is not retained by the current schema, so the revision list proves
commitment continuity but cannot reconstruct an old document.

## Editorial state machine

```text
Draft -> Approved -> Implemented
   |         
   +-> Rejected
   +-> Rework -> Draft
```

`Rejected` and `Implemented` are terminal. Direct `Draft -> Implemented`,
`Rework -> Approved`, self-transitions, and all other unlisted edges are rejected.
Text is editable only in `Draft` and `Rework`.

## Atomicity, replay and failure

Each precompile command is one EVM transaction and must atomically update the
primary record, the old/new status indexes, counters and emitted event. Validation
failure leaves all of them unchanged. Repeating a submission allocates a distinct
proposal; updates therefore are not idempotent unless the caller first reconciles
by id. Revision and proposal counters currently use ordinary increment arithmetic,
so overflow behavior is an architectural precondition rather than a closed error.

## Security and compatibility

The authority set is a governance trust root and must be initialized consistently
at genesis. Because authority addition is permanent in the inspected interface,
key compromise has no on-chain revocation path. Raw numeric statuses and stored
record layouts are consensus state and require versioned migration before their
encoding changes.

## Production-interface evidence

Evidence inspected in `crates/core/governance/src`: schema/storage mappings,
precompile dispatch, proposal and canon mutation paths, status transition graph,
pagination queries and unit tests. Structural closure still requires tests that
compare every primary record with every author/status index after each mutation
and after injected failures.

## Consequences and rejected alternatives

This boundary keeps narrative governance searchable without pretending it can
change running code. Keeping Governance, Vote and Update in one state machine was
rejected: their actors, failure domains, clocks and terminal outcomes differ.
Treating an editorial `Approved` record as executable authorization was rejected
because it bypasses validator eligibility and target-specific payload validation.

## Open questions and technical debt

- Define authority removal, rotation, threshold and compromised-key recovery.
- Replace unchecked counter increments with explicit exhaustion behavior.
- Decide whether historical canon/meta-canon bytes must be retrievable, not only
  their hashes.
- Add exhaustive primary-record versus author/status-index closure tests with
  rollback injection.
- Specify whether OIP and GIP are intentionally structurally identical or need
  distinct domain invariants before their schemas diverge accidentally.
- Define a signed linkage, if any, between an editorial proposal and an executable
  Vote payload; text similarity must never be treated as authorization.
