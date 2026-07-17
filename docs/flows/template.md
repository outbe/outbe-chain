# PFS-NNN: Outcome-oriented title

- **Status:** Draft
- **Actors:**
- **Trigger:**
- **Topology/services:**
- **Referenced ADRs:**
- **Supersedes:** None

## Outcome

One sentence describing the externally meaningful completed result.

## Preconditions and canonical inputs

List chain state, identities, finalized height, external services, keys/proofs and
which source supplies every time, price, amount and id.

## Success sequence

| Step | Owner | Command/effect | Durable evidence |
|---:|---|---|---|
| 1 | module | action | receipt/state/proof |

## Boundaries and conservation

Name EVM transaction boundaries, system-transaction checkpoints, finality and
off-chain materialization checkpoints. State equations spanning modules.

## Observable completion contract

Define exact receipt, ABI/RPC, event, projection and proof assertions. Say which
observation is authoritative if layers disagree.

## Replay, retry, restart and failure

Define retry keys, expected no-ops/reverts, rollback, node restart, delayed external
service and reorg behavior.

## E2E scenario matrix

| Id | Scenario | Minimum topology | Required assertions | Automated by |
|---|---|---|---|---|
| PFS-NNN-01 | happy path | | | GAP |

## Open questions and technical debt

- Every known missing behavior, test or decision; never an empty placeholder without
  a completion audit.

