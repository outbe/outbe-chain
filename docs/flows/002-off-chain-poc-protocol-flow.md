# Flow 002: Tribute to usable NOD through OCOMP

- **Status:** Normative
- **Actors:** Cycle, Metadosis, ACTIVE validators, FullNodes, OCOMP ExEx,
  Supervisors, Workers, txpool/EVM, Lysis, Tribute, NOD, and NodFactory
- **Referenced decisions:** ADR-C-LYS-001, ADR-C-NOD-002, ADR-S-OCM-004

## Outcome

One eligible sealed WorldwideDay is computed by OCOMP, certified by the pinned
validator quorum, and materialized into ordinary NOD ledger entries. Owners can
enumerate, read, approve, transfer where supported, and mine those NODs through
the existing public ABI.

## End-to-end sequence

1. Users submit public Tribute offers during the configured Metadosis window.
2. Cycle advances the WorldwideDay from genesis-bound timing parameters.
3. Metadosis seals the authenticated Tribute population and creates
   `JobIntentV1`.
4. The job snapshots the complete ordered ACTIVE ValidatorSet and its OCOMP
   keys, derives `N`, and derives the shared quorum.
5. `OffchainJobRequested(intent_id)` wakes each node. Supervisors read the full
   finalized intent from chain state.
6. Validators and FullNodes export the finalized inputs and run the same Lysis
   program through production Workers.
7. Validator Supervisors sign and submit full result votes. FullNodes do not
   vote; they compare their local result with the canonical result.
8. The quorum-forming vote atomically installs the typed Lysis effects and one
   certified NOD generation, then enqueues that generation for materialization.
9. A finalized proposer wake asks its local Supervisor to inspect the canonical
   FIFO head. A retry wake is sent after the configured no-progress interval.
10. The Supervisor reads the next result chunk and exact upper Merkle siblings,
    builds one bounded batch, and submits an OCOMP materialization carrier.
11. Every node verifies the same FIFO cursor, ordered actions, identities,
    economics, and shared root path. NodFactory calls `issue_nod` for every
    action inside one checkpoint.
12. Successful batches advance the cursor. The final batch dequeues the
    generation while preserving its certified projection.
13. After completion, owners use the ordinary NOD ABI, including
    `tokenOfOwnerByIndex`, `nodData`, and `mineGratis`.

## Canonical authority

The following are authoritative:

- finalized `JobIntentV1` and its pinned ValidatorSet snapshot;
- the quorum-selected `LysisResultV1`;
- the certified NOD projection and `nod_root`;
- the FIFO head and `next_nod_ordinal`;
- the current ACTIVE OCOMP role for materialization submission.

Events and local wakes are hints only. Supervisor files, process liveness,
proposer identity, and transaction arrival order cannot override canonical
state.

## Materialization batch

The resolved genesis profile defines:

- `batch_subtree_height` and therefore batch capacity;
- `retry_interval_blocks`;
- `max_attempts_per_block`.

The default profile is height 3, retry 30 blocks, and one attempt per block. A
nonfinal batch has exact capacity. The final batch has the exact remaining
actions and canonical padding. The shared `root_path` contains only siblings
above the batch subtree; direction is derived from the global ordinal.

The complete batch is atomic. A failed item cannot leave partial NOD records,
owner indexes, buckets, events, or cursor progress.

## Failure behavior

- Missing local artifacts: that Supervisor abstains; consensus continues.
- Lost or duplicate wake: the retry path rereads canonical state.
- Competing valid batches: the first valid cursor transition succeeds; stale
  copies fail with status zero.
- Bad proof, order, count, root, duplicate NOD, or excess same-block attempt:
  failed receipt and full rollback; the block continues.
- Unauthorized signer or malformed carrier envelope: invalid for inclusion.
- Canonical queue corruption: fatal.
- Certified but incomplete generation: `mineGratis` is rejected.
- FullNode local Lysis mismatch: the FullNode stops.

## Restart behavior

Chain state restores the job, generation, FIFO, cursor, and attempt state. The
Supervisor restores the durable job reference and reconstructs only the current
bounded proof from verified artifacts. Restart does not create a new generation,
duplicate a queue entry, or skip an ordinal.

## Release acceptance

The release E2E uses real `gramine-sgx` through `sudo`, no DCAP/QVL, remote
attestation disabled, chain ID `54322345`, and `--tee sgx-no-attest`. It must:

- submit distinct-owner Tributes no faster than two per block;
- create more NODs than one materialization batch;
- use real Metadosis, Workers, vote transactions, and quorum activation;
- observe at least two materialization transactions;
- prove mining rejection before completion;
- restart and preserve progress; and
- prove post-completion owner enumeration, `nodData`, and `mineGratis` without
  direct result or state injection.
