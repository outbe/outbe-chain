# ADR implementation backlog

This file tracks design work that is intentionally not yet implemented. An ADR's
presence here must not be read as production support.

## ADR-B-CNS-004 — Persistent DKG failure halt and recovery

- **State:** design proposed; implementation deferred
- Define and approve finalized failure attestations and deterministic attempt windows.
- Prove the unique canonical reveal threshold for the active VRF material version.
- Implement persisted halted supervision with diagnostic RPC kept available.
- Implement deterministic quorum recovery manifest, durable signer anti-equivocation
  and the recovery-block FSM.
- Add operator CLI/runbook and focused mock plus hardware-SGX E2E evidence listed
  in ADR-B-CNS-004.
- Remove this entry only after implementation, activation policy, complete evidence
  and ADR status update land together.
