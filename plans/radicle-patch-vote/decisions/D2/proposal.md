# D2 — coherent managed publication when Git ref updates fail

Status: PROPOSED, not implemented or approved. This is a new concrete publication/recovery implementation decision within P2's existing gate, distinct from withdrawn D1. Three independent decision checks precede a concrete user proposal; wait for explicit approval before dependent production implementation. No acceptance gate is replaced.

## Problem and evidence boundary

P0 design §1/§4 requires a complete authorized snapshot, latest-only Promote, local CAS and a monotonic local receipt sequence. An accepted patch/history stays accepted. The current native fetch backend applies refs one at a time (`radicle-fetch/src/git/repository.rs::update/direct`) and has no managed publication transaction/gate. A failure can leave physical refs from different snapshots. Merely replacing this loop with git2::Transaction is insufficient: the installed git2 0.21.0 documentation and libgit2 1.9.7 source explicitly apply changes one by one without rolling back completed operations.

`repro/` calls the real installed git2 transaction API on actual commits/refs, removes one prepared lock file as filesystem fault injection, and observes an error with one new ref and one old ref. It tries both fault positions to avoid relying on hash-map commit order. This is a real low-level partial-commit counterexample. It is NOT a Vote bypass, old-shares attack, delegation feature, or complete runtime/managed-fetch exploit. The production fetch primitive is source-inspected; it is not directly called by this executable because that module is crate-private.

The same executable includes a deliberately reduced journal/read-lease model. It proves only local filesystem leases, durable pending/published records, detection/gating of a partial physical view, retained immutable archive data, and idempotent restart roll-forward. Its accepted target check is a fixture, NOT an Outbe chain verifier, actual upload-pack guard, or production implementation. It does not justify closing P2.

## Recommended implementation: journal + complete-view gate

Reuse native Git refs and native signature formats. For a managed owner only, serialize installation through one per-owner cross-process publication write lease. Before mutation, verify the already specified trusted Promote ticket against exact repo/owner/manifest/new complete refs and actual local refs root. Revalidate the ticket/local CAS while holding the write lease.

Persist and fsync a bounded pending journal before changing the first ref. The journal commits the last complete accepted snapshot/receipt floor, actual expected local root, intended complete snapshot and its authenticated receipt/sequence/finalized block. Objects remain immutable. The durable complete-view pointer and monotonic floor advance together only after all required ref updates/deletes and their durability checks succeed and the resulting full snapshot is reverified.

Current managed views, canonical ref consumers, cache materialization and Git export must use a publication read lease for the whole mutable snapshot use/export, check readiness and bind the same complete-view root. A check before spawning upload-pack alone is insufficient: hold the lease until the child finishes. A paused reader must not observe two snapshots; a pending/failed writer must not expose a mixed physical ref set. Low-level raw filesystem access by a trusted local operator is not a product/network authorization API.

On write failure/crash, retain the pending journal and leave the complete-view pointer/floor at its last whole value. Do not report the new snapshot as ready or mark execution published. Reopen/startup and each managed entry point detect pending state. Recovery revalidates trusted finality/latest receipt, obtains missing immutable objects outside the write lease, reacquires the lease/CAS and resumes idempotently to the authorized latest complete snapshot. If a newer receipt superseded the target, use that latest complete snapshot; do not briefly promote the old target. The gate stays closed for current owner views/export while physical state is incomplete. An explicit temporary unavailable/busy result is preferable to exposing mixed refs. This is the observable availability tradeoff requiring approval.

Archive remains separate from Promote. Exact previously accepted immutable objects and their accepted receipts remain valid, including during recovery or membership changes. No accepted Vote, patch or historical receipt is revoked or retallied. A stale generation does not gain a new write right. No forced new Vote is introduced for an I/O retry. Ordinary healthy Ed repos retain their existing path. In managed repos, ordinary Ed authors/own refs remain native; a denied owner branch fails before it can create a publication journal or block independent author work. Any operation requiring an unavailable current owner view may wait/retry while a legitimate local storage failure is recovered; consensus must not wait on this.

No new network signature, DKG rule, Vote rule, owner key, user delegation, or global Git backend is introduced. Heartwood remains generic: only opaque trusted receipts/tickets and managed owner configuration. The actual Outbe policy/finality verifier stays outside Heartwood. Journal schema is local versioned bounded storage, not a chain/wire protocol change. Existing native objects and Ed encodings stay compatible.

## Mandatory integration map

- `radicle/src/storage/{publication.rs,git.rs,git/cob.rs,temp.rs}`: publication lease/journal/complete-view metadata, actual local mutation and canonical read boundaries; all bypassing raw-ref callers must be accounted for before editing.
- `radicle-fetch/src/{state.rs,git/repository.rs}` and temporary repo install: authorize and install complete managed owner snapshot through the gate; do not alter ordinary peer behavior globally.
- `radicle-node/src/{worker.rs,wire.rs,runtime.rs}` and worker upload/transport process lifetime: export lease/readiness, bounded busy/retry, cleanup and restart.
- `radicle/src/cob/{store.rs,stream.rs,patch/cache.rs,issue/cache.rs}`, identity/canonical refs and CLI/helper consumers: current reads/materialization bind one complete view; immutable Archive lookups use accepted provenance. D1 full dependency/cache design remains separately unapproved; this proposal does not authorize D1 by implication.
- Outbe policy bridge in later P3/P6: exact finalized receipt/current latest snapshot; no Git/network/wait inside consensus execution. P2 tests may use an explicit test provider but cannot claim the production Outbe bridge is complete.

The implementation must map actual constructors/callers and use existing helpers. Exact Rust names and physical journal filename are implementation details; do not introduce extra configurable modes or infrastructure.

## Alternatives considered

1. Bare git2::Transaction: smaller but fails the actual fault counterexample and lacks a durable complete-view/sequence floor. Not a valid solution alone.
2. New ref storage backend/reftable, manually managed packed-refs or virtual Git export snapshots: potentially provide a different atomic publication boundary, but require wider Git compatibility/storage/export changes. Prefer only if the review demonstrates the journal/read-lease approach cannot meet the existing contract with a smaller complete change. Do not silently select an alternative or weaken required reader/export coverage.

## Required proof before eventual P2 closure

Actual local/fetch publication and second-node reads with a real managed owner; exact ticket/manifest/CAS checks; fail after every durable transition/ref mutation; no mixed current view/export/cache; whole accepted archive unchanged; reopen/recovery, competing writers and a live reader/upload; latest/sequence/hash conflicts and no-op material binding; missing/untrusted policy fails closed; healthy pure author activity and static BLS/legacy Ed remain valid. Bounded waits/no reactor signature wait/no consensus I/O. Full existing suites and three independent all-AC acceptance reviews still required.

## Independent decision review request

Each non-author reviewer inspects the frozen source/contracts and this proposal, reproduces the primitive counterexample/model, and checks the actual current/local/fetch/export/cache callers. Assess whether the problem and decision are real, whether the recommendation stays inside P2/P0 and the user's sole-BLS/no-delegation/immutable-history contract, and whether its complete read/export lease and recovery model is feasible. Identify required corrections and any contradiction; do not hide unverified production behavior behind the reduced model. Do not read peer reports, edit source/spec/proposal/repro or close Beads. Write only own report/logs. Coordinator presents the result and waits for explicit user approval; a review PASS is not permission to implement.
