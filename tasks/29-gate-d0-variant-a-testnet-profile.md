# T29 — Gate D0: Variant A body reads (Stage 1 testnet, minimal profile)

Status: todo
Source: `audit_plan.md` §1 (owner decision), §4 P0-1; concept §3.2 Stage 1 note; SCOPE RE-CUT 2026-07-13
(owner decision: the goal of CES is DATA PRESERVATION — execution-read choreography is out of scope; the
former readiness machine / coverage gates / automatic recovery are removed from Stage 1)
Depends on: —  (decision/spec gate; no code prerequisites)
Blocks: T09 (Lysis-first ordering), T20 (domain-adapters part), T23, T27, T30, T33, T34, T35, T36

## Summary

After T27 removes legacy per-record EVM bodies, body-dependent Tribute/Nod operations (Lysis partition
lists; point reads in NodFactory mine_gratis, Tribute burn/processing) read canonical bodies from the
validator-local MongoDB projection. This is a RECORDED OWNER DECISION and a testnet-only exception to the
concept's "MongoDB is never a state-transition input" rule — a forced consequence of deleting legacy
storage, not a goal. Stage 1 keeps this path DELIBERATELY DUMB: read → verify → fail. No readiness
choreography, no coverage accounting, no automatic recovery.

## Contract (normative for Stage 1 implementations)

1. Profile gating: Variant A activates only for testnet chain/profile. Production/mainnet startup with
   Variant A enabled terminates with a structured error — hard disable, not a warning.
2. Topology: every validator deployment runs its OWN MongoDB (a separate container in the same deployment
   counts as local); a shared/external Mongo serving multiple validators is forbidden;
   `--ce-body-service=external` on a Variant A validator is a startup error. Full nodes may run
   split/external topologies.
3. Read rule (the whole rule): fetch by canonical identity/partition; every consumed body re-derives
   identity → `tree_key` → `leaf_value` and verifies against the executing parent's CES commitment BEFORE
   use. A row that is missing, corrupt, stale, or mismatched ⇒ THE READING OPERATION FAILS
   (`BodyReadFailed`, node-local). Partition lists are read in canonical identity order;
   duplicates/malformed rows fail the read. List completeness has no proof — an undetected omission is an
   accepted testnet risk.
4. Same-block ban: after a successful mutation (or partition retirement), any further body-dependent
   operation on that entity/partition in the same block fails via T07's journaled predicates
   (`entity_mutated_this_block`, `collection_retired_this_block`) — typed `SameBlockBodyUnavailable`, an
   ordinary deterministic domain revert; a reverted mutation/retirement creates no ban.
5. Failure model (owner decision, scope re-cut): a node with missing or wrong body data computes a
   diverging result and FALLS OUT of certification (its votes don't match / its proposals die); the
   candidate is never consensus-invalid and the network continues on quorum. There is NO readiness state
   machine, NO coverage gate, NO automatic catch-up. Recovery is operator-driven: resync or restore per
   the T34 runbook. A full node with a data gap stalls its own import the same way — operator fixes it.
6. Honest telemetry: metrics and warnings state explicitly that no completeness proof exists for list
   reads; a diverging node is visible through metrics (failed reads, root mismatches), not through a
   protocol state.
7. Release discipline: nothing proven under Variant A is production/mainnet activation evidence.
   Production requires a separately designed off-chain computation path with its own release gate.

Additionally (pairs with P0-2): all Lysis CES mutations execute through receipt-visible system
transactions — never raw hooks (producer inventory owned by T09); the Lysis system phase executes before
user transactions and before any other CE-mutating system work on the same partitions
(hard-fork-governed executor order; executable gate in T09).

## Accepted testnet risks (recorded)

- Silent divergence: a node with a silent data gap does not know it is broken — it stops certifying and is
  visible only in metrics.
- A quorum-level Mongo loss halts the testnet (exercised as a T34 scenario).
- Recovery is manual only (resync / snapshot restore per runbook).

## Acceptance criteria (gate-artifact completion — own deliverables ONLY; downstream compliance is
verified by the implementing tasks, per the roadmap gate convention)

1. Contract items 1–7 + accepted risks published; the concept §3.2 Stage 1 note matches this minimal
   model; T20/T23/T25/T27/T33 reference them.
2. Typed outcome names (`BodyReadFailed`, `SameBlockBodyUnavailable` — naming handed to T30) and the
   startup profile-gating spec published.

(Downstream compliance — production-startup failure test, tamper/missing/stale-row fixtures — is owned by
T33/T20 Part B acceptance criteria, NOT by this gate.)

## Invariants

- Consensus state transitions never read Mongo outside this explicitly gated testnet profile.
- The exception is visible: profile flag, startup log, and metrics all name Variant A.
- Mongo is used, never trusted: every consumed body verifies against the CES commitment.
