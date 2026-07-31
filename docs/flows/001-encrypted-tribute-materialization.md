# PFS-001: Encrypted Tribute offer becomes finalized, projected and authenticated

- **Status:** Draft
- **Actors:** Tribute creator, validator committee, enclave sidecars, Mongo projector
- **Trigger:** Creator submits `offerTribute`
- **Topology/services:** Four-validator TEE-capable localnet; one enclave per
  validator; transaction-capable MongoDB deployment with one logical database per
  validator
- **Referenced ADRs:** ADR-B-GEN-001, ADR-B-CNS-002, ADR-B-CNS-003,
  ADR-B-CLI-001, ADR-B-MCP-001,
  ADR-B-OCD-004, ADR-B-OCD-005, ADR-S-TEE-001, ADR-S-TEE-002, ADR-C-TRB-001, ADR-C-TRB-002
- **Supersedes:** None

## Outcome

One encrypted offer executes deterministically on every validator, produces one
canonical Tribute, appears identically in each validator's Mongo projection, and is
retrievable with a compressed-entity presence proof against a finalized header.

## Acceptance contract

- **Source:** Tribute creator using transaction-capable client or operator tooling.
- **Trigger:** A creator submits one `offerTribute` transaction encrypted for the committee offer-key epoch.
- **Environment:** Four-validator finalizing localnet with active committee offer key, open WorldwideDay, Oracle state, enclaves and validator-isolated Mongo projections.
- **Canonical inputs:** Creator/sender identity and nonce, encrypted canonical offer bytes, chain id, WorldwideDay, currency, amount, issuance flag, L2Registry registration state for the sender plus the offer's `zkMerkleRoot`/`signature` pair, finalized block time, Oracle state and enclave-held decryption key.
- **System under test:** TributeFactory, Tribute/CE execution, consensus finality, projection pipeline and CE read/proof RPC.
- **Expected response:** Finalized receipt and CE event, canonical Tribute identity/body, per-validator Mongo projection/checkpoint, and a presence proof anchored to a finalized header.
- **Response measures:** Exactly one Tribute and one supply/totals increment exist; all validators agree on execution and authenticated body; every projection matches it within the scenario timeout; the proof verifies against the selected finalized header.
- **Failure guarantee:** Rejected ciphertext or replay creates no Tribute, projection, totals change or CE intent; projection/restart retry never re-executes the transaction.

## Preconditions and canonical inputs

- The four-validator committee has finalized blocks and completed the TEE offer-key
  bootstrap; every enclave reports the same derived public offer key.
- The WorldwideDay is in OFFERING and its Tribute partition is unsealed.
- The creator controls the transaction key. Public/encrypted identity binding
  follows ADR-C-TRB-002 and its unresolved envelope-binding debt.
- Oracle currency/rate input required by TributeFactory exists at the execution
  block.
- L2 zk signature gate: if the sender is registered in the L2Registry
  (`0x…EE0E`) as a network's L1 operator address and that network has
  `zk_enabled` set, the offer must carry a non-empty `zkMerkleRoot` and a valid
  BLS MinPk `signature` over it (namespace `_OUTBE_L2_ZK_MERKLE_ROOT`) from the
  network's registered key; the gate rejects before any Oracle/enclave work.
  Unregistered senders and zk-disabled networks bypass the gate with empty
  bytes.
- Each node uses a distinct projection database but the same replica-set service is
  allowed; database isolation is logical, not one Mongo server per validator.
- Canonical inputs are encrypted offer bytes, chain id, transaction sender/nonce,
  finalized block time, Oracle state and enclave resident key.

## Success sequence

| Step | Owner | Command/effect | Durable evidence |
|---:|---|---|---|
| 1 | client/enclave tooling | derive/read committee offer key and encrypt canonical offer payload | ciphertext and key id |
| 2 | creator | submit `offerTribute` | transaction hash only |
| 3 | TributeFactory + enclave | decrypt/validate and create Tribute/CE mutation | successful canonical receipt and event |
| 4 | consensus | finalize containing block and its state/CE roots | finalized header from each validator |
| 5 | projection pipeline | consume finalized CE effects into per-node Mongo | checkpoint plus canonical BSON document |
| 6 | CE RPC | return presence proof for Tribute identity | proof package and selected finalized header |
| 7 | independent verifier | verify proof and compare authenticated body with canonical Mongo bytes | verifier success and byte equality |

## Boundaries and conservation

Submission is not completion. Steps 3 is one EVM transaction; failure leaves no
Tribute or CE intent. Finality is a later consensus boundary. Projection and proof
availability are post-finality materialization boundaries and must be retryable
without re-executing the transaction.

Exactly one successful offer creates one Tribute identity, increments the matching
day totals/supply once and produces one canonical body. All validators must agree on
receipt, state root and authenticated body. Mongo documents may contain projection
metadata, but their canonical domain fields must be identical.

## Observable completion contract

Completion requires all of:

- receipt status success in a finalized block;
- ABI/RPC lookup returns the created Tribute under its canonical owner/day identity;
- all four logical Mongo databases contain one document with matching transaction
  hash and identical canonical BSON;
- each validator returns a verifiable `Present` proof against an exact finalized
  header; proof packages may select different sufficiently finalized headers;
- authenticated body bytes equal the canonical projected body bytes.

If Mongo and verified proof disagree, finalized authenticated state is authority and
the projection is corrupt/stale.

## Replay, retry, restart and failure

A reverted offer must create no state or projection. Re-submitting an identical
logical identity must follow Tribute uniqueness rules, not create a duplicate.
Projection retries use finalized block/checkpoint identity and must be idempotent.
Restarting any validator or Mongo after finality must converge without replaying the
user transaction. Enclave unavailability makes execution fail consistently; it may
not produce validator-specific receipts.

## E2E scenario matrix

| Id | Scenario | Given / canonical inputs | When / trigger | Then / outputs and postconditions | Verification |
|---|---|---|---|---|---|
| PFS-001-01 | encrypted offer happy path | 4 validators, active mock TEE key, open day, valid Oracle and canonical offer | creator submits `offerTribute` | finalized success; one authenticated Tribute; equal projections/proofs on committee | `@pfs-001-01` live-node |
| PFS-001-02 | unknown identity in existing day | finalized existing Tribute collection and synthetic absent id | query point proof on every validator | verified `EntityAbsentInCollection`; no matching projection | `@pfs-001-02` live-node |
| PFS-001-03 | unknown day | finalized chain and synthetic never-created day/id | query point proof on every validator | verified `CollectionAbsent`; no primary/secondary projection | `@pfs-001-03` live-node |
| PFS-001-04 | invalid ciphertext/proof | open day and byte-invalid encrypted envelope | submit malformed offer | reverted receipt; totals/body/projection unchanged | documentation-only: CLI blocks malformed envelope construction |
| PFS-001-05 | duplicate logical owner/day offer | one finalized offer for owner/day | owner submits newly encrypted offer for same identity | reverted duplicate; supply and all projections remain exactly one | `@pfs-001-05` live-node |
| PFS-001-06 | Mongo outage and recovery | healthy finalizing chain; projection replica set paused | finalize a valid offer, then restore Mongo | chain progresses; projector catches up once with matching checkpoint/body | documentation-only: no Mongo pause/resume seam |
| PFS-001-07 | restart before projection | finalized offer and projector stopped before checkpoint | restart validator/projector | checkpoint recovery converges to four-way equality without resubmission | documentation-only: no deterministic projection failpoint |
| PFS-001-08 | enclave unavailable | one executor/proposer enclave unavailable, otherwise valid offer | submit while affected node handles execution | deterministic failure/retry; no validator receipt/root divergence or projection | documentation-only pending retry policy and enclave fault control |
| PFS-001-09 | identical encrypted-envelope replay | retained exact envelope and fresh valid transaction nonce | resubmit byte-identical encrypted envelope | replay rejected; identity/totals/projection unchanged | documentation-only: harness cannot retain raw envelope |
| PFS-001-10 | zk-enabled L2 operator without signature | sender registered in L2Registry with `zk_enabled=true`; open day and valid canonical offer | submit offer with empty `zkMerkleRoot`/`signature`; then disable zk and resubmit | first offer reverts with zero supply/projection change; after `setZkEnabled(false)` an unsigned offer succeeds | `@pfs-001-10` live-node |
| PFS-001-11 | zk-enabled L2 operator with valid signature | sender registered with `zk_enabled=true`; `zkMerkleRoot` signed by the registered BLS MinPk network key | submit offer carrying root + signature | gate passes; finalized success and one authenticated Tribute | `@pfs-001-11` live-node |

## Open questions and technical debt

- Report the existing PFS tags in CI and add tags for future outage/restart/enclave scenarios.
- Current encrypted creator semantics and public sender/AAD binding remain open in
  ADR-C-TRB-002.
- Production attestation remains weaker than the intended trust claim in ADR-S-TEE-001.
- Define projection service-level completion timeout without making wall time part of
  consensus correctness.
- Add restart, Mongo outage, malformed ciphertext and byte-identical encrypted-envelope replay scenarios.
- Reconcile which BSON bytes are canonical proof body versus projection-only
  metadata with a versioned codec test.
