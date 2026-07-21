# ADR-B-CNS-004: Persistent DKG failure requires a canonical halt and quorum-authorized recovery

- **Status:** Proposed; protocol and implementation work deferred
- **Date:** 2026-07-21
- **Scope:** validator-committee DKG failure accounting, consensus halt, diagnostic operation and recovery
- **Depends on:** ADR-B-CNS-001, ADR-B-CNS-002, ADR-B-CRY-001, ADR-B-NOD-001, ADR-B-SUP-001

## Context

Current reshare retries the same frozen target while outgoing VRF material remains
valid and fails closed at its published expiry height. It does not retry forever.
At expiry the consensus stack returns a fatal error and the validator process
exits, so there is no production recovery protocol and no guarantee that
diagnostic RPC remains available.

Repeated retry count alone does not prove VRF compromise. Durable retry reuses the
same ceremony identity and byte-identical dealer transcript. Any emergency reveal
rule must count unique canonical revealed evaluations for one material version and
be justified against the threshold scheme.

## Proposed decision

A future protocol version shall introduce a persisted `Halted` consensus state,
entered by one deterministic finalized condition rather than a local timer. The
condition may combine a finalized failure budget with an independently justified
unique-reveal threshold. Proposal and vote production stop at one finalized halt
checkpoint while diagnostic RPC and operator observability remain available.

Recovery may resume only the exact frozen target. It requires a deterministic
recovery manifest authorized by the current halted committee quorum using
domain-separated individual consensus BLS signatures. The quorum is computed by
the same committee policy as consensus; it is not a separately hard-coded literal.
Each signer durably signs at most one manifest per halt checkpoint and recovery
generation.

The manifest binds chain ID, finalized halt height/hash, frozen-target hash,
recovery generation, next deterministic attempt, protocol version and schema
version. It has no arbitrary nonce. Nodes persist acceptance before participating,
reject conflicts and initially permit only a special recovery block. Ordinary
proposal/vote/DKG operation resumes only after that block finalizes and commits its
certificate hash.

## Required protocol work before acceptance

1. Define how local ceremony failure becomes canonical, for example through
   domain-separated failure attestations and a deterministic finalized attempt
   boundary.
2. Define production attempt windows. A count of 24 is not equivalent to one day
   unless consensus defines hourly windows; current ceremony timeout is shorter.
3. Prove the emergency reveal threshold over unique canonical identities,
   transcript and material version.
4. Specify deterministic certificate encoding, signer uniqueness/membership,
   durable anti-equivocation and recovery-block verification.
5. Specify a supervisor state in which consensus is halted but diagnostic RPC is
   alive, including readiness and operator-visible reason.
6. Specify crash consistency for signer intent, accepted certificate, conflict
   record, recovery block and resumed DKG material.

## Required verification

- divergent local timeouts cannot increment a chain-wide counter independently;
- all nodes finalize identical failure count and halt checkpoint;
- no proposal/vote/block occurs after halt while diagnostic RPC remains usable;
- insufficient quorum, wrong chain/checkpoint/target/version and replay reject;
- conflicting manifests cannot partially resume the committee;
- signer restart cannot authorize two manifests for one generation;
- valid recovery resumes the same frozen target, finalizes one recovery block and
  activates new VRF material exactly once;
- crash injection covers every persistence boundary;
- hardware-SGX coverage is required wherever recovery touches enclave-resident state.

## Consequences

This is a new protocol feature, not a correction for infinite retry. Until it is
implemented and activated, ADR-B-CNS-002's VRF-expiry process termination remains
the normative fail-closed behavior.

## Open questions and technical debt

- Exact finalized failure-attestation format and hourly schedule are undecided.
- The unique-reveal emergency threshold lacks a written cryptographic proof.
- Recovery certificate codec/domain and special recovery-block FSM are not implemented.
- Node supervision currently exits with the consensus task and cannot preserve diagnostic RPC.
- Operator signing/distribution tooling and runbook do not exist.
