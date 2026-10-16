# Automatic bootstrap collector — intermediate P3/P4/P6 evidence

Source `3181fe773e85710412e7927a3ab82692d673f315`, base `e850e5a74d47855b33bdd3324271b9fa27fa6ea8`.
`checks.json` records the exact source/spec hashes, commands/cwd, Rust/platform,
environment, UTC times, exits and log hashes. All 156 affected SDK/runtime tests
passed, with zero ignored. Seven consumers built on all targets; SDK/runtime/node
all-target Clippy with warnings denied and formatting passed. Source was unchanged
during these checks. This is intermediate evidence, not full-AC task acceptance.

## Callers and wiring

| Required behavior | Production source | Behavioral evidence |
| --- | --- | --- |
| Automatically start post-finality collection with the native sidecar | `crates/system/radicle/src/lib.rs:424`, `collector.rs:551` | Actual `PreparedSidecar::run` in `tests/governments.rs:146`; no direct test invocation of collector steps |
| Obtain chain identity from the protected local node, not a public peer | SDK `integration/local.rs:258`, existing same-UID/private-socket checks | Actual OCN1 exchange in the SDK socket test and automatic sidecars |
| Sign through the existing provider; recover with public data only | SDK `integration/signing.rs:276`, existing OCB/OCJ/OCI/OCF services | Automatic three-subject native BLS recovery; existing missing-provider/current-material negatives in the full SDK suite |
| Verify and persist indexed public partials before counting or retransmitting | runtime `collector.rs:371`, `collector/journal.rs:95` | Initially only two endpoint actors run: both journal their shares, no batch or receipt; the third actor later makes progress while member four remains offline |
| Persist complete package, obtain current attestations and advance bounded batches from own-finalized progress | runtime `collector.rs:184` | Actual OCA/OCQ/OCG/OCT/OCL services, endpoint actors and registry `apply_publication_batch`; no manually injected ready batch or recovered signature |
| No native owner grants before accepted proof | SDK `integration/publication/policy.rs:63` | Existing native bootstrap fixture denies identity lookup before accepted receipt/proof |
| Install only exact accepted native refs, recover already-new refs and refuse a conflicting value | SDK `integration/bootstrap.rs:511` | Bootstrap test inserts a conflicting main ref: install rejects and only that one ref remains; then existing interrupted-state and repeated install pass |
| Resume a collector before quorum and restore accepted native policy after restart | runtime `collector.rs:464`, `collector/journal.rs:16` | Actual sidecar stops/restarts after its document partial; later stops/restarts after acceptance, and live native storage again exposes the accepted RID |
| Announce through the existing native runtime; ordinary native fetch remains usable | runtime `collector.rs:464` | Live HTTP availability, equal native heads on three nodes, actual Noise connection and successful native fetch containing the BLS namespace |
| Journal ownership, pinned chain and committed file preservation | `collector/journal.rs:146` | Three journal tests: second writer rejected, reopen same chain succeeds/different genesis rejects; interrupted temporary write preserves committed record; immutable conflict, nonprivate directory, symlink and oversized file reject |

All paths in the table are relative to `crates/system/radicle/src/` unless prefixed
with SDK or a complete repository path. SDK paths are relative to
`crates/blockchain/radicle/src/`.

## What the multi-node test proves

`automatic_bootstrap_reaches_quorum_with_one_offline_member_and_restarts` starts
three real native sidecars with transport Ed25519 keys and three real Outbe endpoint
actors. Each node has a separate existing `VrfMaterialProvider` with its own share,
separate protected Unix socket and separate node/runtime package stores. The test
does not create a repository owner key or call `rad init`.

The native document, identity COB and signed refs are constructed and recovered
automatically by the workers. Current MinPk attestations are produced by actual SDK
services; the fixture forwards ready mailbox batches to the real registry admission
method and exposes immutable canonical finalized states through the production
`RethSnapshotReader`. The test observes three accepted installations, identical
native heads, native fetch and restart restoration.

## Evidence boundaries and remaining requirements

- The DKG dealer, Commonware authenticated transport and canonical block finalizer
  are fixtures (`tests/governments/support.rs`); this is not interactive production
  DKG, Simplex consensus, EVM block execution or the full P8 E2E.
- The test covers bootstrap with empty initial content. Nonempty content still
  requires the exact pre-genesis artifact and Git content to be available locally;
  there is no new automatic content-distribution claim.
- A collector with no locally selected accepted package returns
  `AwaitingAcceptedPackage`. Fresh-node archive/package delivery is incomplete.
- Only bootstrap is automatically scheduled. BindMaterial, approved-job discovery,
  Review/Merge scheduling and full rotation/window/budget/recovery scenarios remain.
- This run does not replace the required three independent full-AC reviews. P2/P3/P4
  and the feature remain incomplete; no task or review was closed.
- No native `storage/git.rs` mutation, new consensus rule, push, PR or deployment.
