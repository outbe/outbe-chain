# Intermediate P3/P6: durable Review packages and body selection

Frozen source: `2ddfe4a5382511f83d4ba4ae2f31a7a0b4ce591a`. Component base is
`7ba8dfe03688a17a93cabd0aa59777999f616e43`. checks.json records the exact
source/spec hashes, component diff, argv/cwd/env/toolchain/platform, times/exits
and log hashes. All 53 adapter tests, seven consumer all-target checks, adapter
Clippy -D warnings and all component Rust-file formatting checks passed.
This is partial P3/P6 implementation evidence, not full task acceptance.

| Required fragment | Production source | Behavioral evidence |
| --- | --- | --- |
| Full package persisted before publication attestation | publication/store.rs::ReviewPackageStore::persist_and_select, SelectedReviewPublication; network.rs::attest_review | actual native scenario persists verified body/manifest/witness/new native objects before all four local actors can sign; bare VerifiedReviewPublication no longer satisfies the API |
| One writer and immutable body selection per current material/sequence | OS File::try_lock, review_slot_key, private select/selection | selected_slot_survives_reopen_and_cannot_choose_another_body: second opener fails, drop/reopen works, another body is rejected, valid witness substitution leaves original selection intact |
| Data file -> fsync -> atomic rename -> parent sync, including newly created directory ancestors | write_once, private_dir/create_dir_durable | atomic_artifacts_recover_uncommitted_temporary_bytes_without_overwriting_committed_data exercises stale temporary bytes, idempotence, bounded reads and committed mismatch refusal; source establishes sync ordering |
| Restart restoration validates content and never promotes refs | load_selected/load; typed StoredReviewPackage remains untrusted until native verification | actual native scenario closes/reopens store, restores a different Git storage deliberately missing every new native object, revalidates full package with public-only material, refs remain absent |
| Corrupt committed selection must fail before another token | persist_and_select reloads the selected package; strict body/manifest/witness/object checks | extended native scenario corrupts original witness then offers a different valid subset; corrected source rejects. Manifest corruption also rejects. RED on previous runtime is preserved in review-store |
| Private directories and no symlink-target overwrite | private_dir/reject_symlink, bounded read/write_once | symlinks_and_public_store_directories_fail_without_changing_the_target verifies no target modification and no chmod of existing public directory |

## Scope and data contract

This store implements CompletePackagePersisted and the local durable selected-body
prerequisite of PublicationAttesting. It does not implement the full collector FSM.
The lock is held by one store instance; &mut methods serialize mutation inside it.
OS release on process exit is the standard File-lock contract. Writer metadata and
all package files are private. No BLS Share or key is persisted.

The local OLS1 slot key isolates chain/genesis/repo, epoch/version, the canonical
bound material hash/players commitment and publication sequence. Candidate-specific
fields and interchangeable witness bytes do not create another slot. This is a
local storage identifier, not a replacement for the existing chain OPG1 hash or
new consensus/wire rule. Current BindMaterial pins exactly one canonical Output.

Package directories are addressed by OPB1 body hash. The canonical body/manifest,
SHA256-addressed raw native objects, separately hashed canonical witnesses and
ready marker are synced before an OAS1 selected-body/witness record is published.
Repeated valid subsets have the same body; their distinct witness artifacts can
coexist. The original selected pointer is retained and must remain loadable.
A corrupted committed artifact fails closed; this API does not silently repair it.

The store rechecks object header/type/size before reading, then raw Git OID and
manifest SHA256. Review native object reads are capped at the existing one-MiB
native payload bound; this Review-only implementation claims no Bootstrap/Merge
content-storage limit. Body, manifest and witness reads have their existing fixed
or one-MiB bounds. Restoring immutable Git objects is not ref promotion. Raw loaded
records have no signing authority; pending jobs must be freshly reconstructed and
verified again to obtain the private Selected token. Its actor still rereads current
finalized permission/material/membership immediately before its identity signature.

## Limits and remaining work

Tests perform real filesystem writes/syncs, reopen/lock checks and native object
restoration, but do not simulate sudden power loss, SIGKILL of a live node, a full
multi-process journal, or normal peer delivery. Directory/file sync order is source
evidence, not a production power-failure measurement. Synthetic slot tests check
filesystem selection semantics; the native scenario uses actual signatures and
Git objects with fixture chain authority/receipts and NoSend transport.

Protected node/sidecar UDS is still unimplemented. The full durable FSM, deterministic
approval queue, received-partial journal, epoch cancellation/WaitingForWindow,
distributed collection/checkpoint delivery, normal ref CAS/announcement/replication,
automatic genesis bootstrap, separate Merge and production E2E remain required.
No task acceptance reviewers/PASS or closures, no git.rs/fetch-gate/D2 changes.
