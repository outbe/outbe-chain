# ADR-C-GRT-003: GratisPool owns shielded Gratis note membership and spend replay

- **Status:** Proposed; current implementation profiled
- **Date:** 2026-07-17
- **Decision owners:** Privacy and Gratis protocol maintainers
- **Scope:** `crates/core/gratispool`
- **Depends on:** ADR-B-CNS-003, ADR-B-EVM-004, ADR-B-CRY-001
- **Related:** ADR-C-GRT-001, ADR-C-GRT-002, ADR-C-PRM-002
- **Supersedes:** GratisPool portions of former broad pre-space Gratis/economic aggregate (previously numbered 030)

## Context

GratisPool hides which pledged denomination note is later used for unpledge or
Credis. It owns authenticated commitment membership, recent roots, nullifier
uniqueness and action/receiver binding. It does not own actual Gratis escrow or
credit positions.

## Decision

Each supported denomination has an incremental Merkle tree and bounded recent-root
history. Commitment uniqueness and spent-nullifier uniqueness are global across
denominations. Appending a commitment updates exactly one tree and root window.

A spend verifies public inputs binding:

- the selected denomination and accepted root;
- nullifier and commitment witness;
- protocol/domain tag and chain id;
- action (`unpledge` or `request_credis`);
- receiver/target account; and
- context nonce defined by the calling workflow.

The pool recomputes receiver binding; it never trusts caller-supplied binding as
authority. Validation and proof verification precede nullifier consumption.
Consumption commits only if the complete outer transaction succeeds.

Credis installment repayment may append a reclaim commitment at the denomination
one decade below the original note. The pool records that commitment opaquely; this
limitation is unresolved below.

## Interfaces and authority

Gratisfactory may append pledge commitments and verify unpledge spends.
CredisFactory may verify Credis spends and append reclaim commitments. These are
the only intended mutation callers and require structural caller tests.

View interfaces expose roots/proof inputs without offering a raw “mark spent” or
arbitrary-root mutation. Proof verification implementation and verifying keys are
imported from ADR-B-CRY-001.

## Persistent state and invariants

- Each tree's leaf count, frontier and root describe the same ordered leaf set.
- A commitment appears at most once globally.
- A nullifier changes from absent to spent at most once globally.
- Every accepted root was produced by the matching denomination tree and lies
  inside the retention window.
- A proof for one action, receiver, chain, nonce or denomination is invalid for any
  other tuple.
- Tree capacity cannot wrap or overwrite a live leaf.
- Root-window eviction is deterministic and preserves exactly the configured
  number/order of roots.

## Atomicity, replay, failure and bounds

Append and spend are transaction-atomic. Invalid/stale root, malformed proof,
duplicate commitment, spent nullifier, unsupported denomination and capacity
exhaustion revert. Corrupt frontier/root history is an invariant failure.

Nullifiers are the durable replay keys. Reorg and failed outer calls roll them back
with EVM state. Root retention creates an intentional spend-expiry window that must
be large enough and normatively documented.

Tree depth, denomination count, root history, proof byte limits and verification
gas are consensus capacity parameters.

## Security, compatibility and evidence

Commitment hash, tree hash, zero values, endianness, domain tags, receiver-binding
encoding, verifying key and denomination ids are consensus cryptographic formats.
Any change requires versioned activation and cross-version vectors.

Runtime paths for add, unpledge spend, Credis spend and reclaim insertion were
inspected. Existing tests cover core proof/root/nullifier behavior, but caller
closure, maximum-capacity behavior and verifiable reclaim denomination are not
closed.

## Consequences

The privacy/replay kernel can be audited independently of Gratis balances and
Credis debt. Calling modules import a precise fact: one valid bound note was
consumed, not that external economic accounting is correct.

## Rejected alternatives

- **Per-denomination nullifier sets:** the same logical note could be replayed under
  a confused denomination encoding.
- **Trust caller receiver binding:** mempool copies could redirect value.
- **Accept any historical root forever:** state and replay surfaces grow unbounded.
- **Store plaintext note ownership:** it defeats the pool's privacy purpose.

## Open questions and technical debt

1. Reclaim commitments are opaque. A commitment created with the wrong
   denomination is accepted but permanently unspendable; add a proof or commitment
   encoding that verifiably binds denomination at insertion.
2. Specify the exact recent-root retention period in user-visible time/blocks and
   analyze censorship/reorg risk for notes near eviction.
3. Prove tree capacity behavior at the final leaf; exhaustion must fail before any
   frontier/index mutation.
4. Add structural caller tests for every mutating internal API.
5. Add cross-action, cross-chain, cross-receiver, cross-denomination and context
   nonce replay vectors at the production ABI.
6. Define commitment/nullifier canonical field bounds and reject non-canonical
   encodings before proof verification.
7. Establish pruning/migration rules for root windows and whether old notes require
   a protocol-supported refresh path.
8. Audit global commitment uniqueness against zero/default sentinels and every tree.
9. Prove failed Credis/vault or unpledge/Gratis effects roll back the consumed
   nullifier and any newly inserted reclaim commitment.
10. Document privacy leakage from denomination, timing, action and receiver binding;
    cryptographic membership does not hide all transaction metadata.
11. Add independent reference-tree vectors and maximum-depth property tests.
12. ADR-B-CRY-001 must pin the exact UltraHonk verifier/key provenance used in production.
