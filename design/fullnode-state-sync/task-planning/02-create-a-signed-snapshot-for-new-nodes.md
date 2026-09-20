# 02. Create a signed snapshot for new nodes

Beads: `outbe-chain-g14b.16`. Baseline: `e5d545cedc872c0e579fd3535e7638d883d5b1f5`.

## Outcome

Create a self-contained mandatory-signed artifact of ready native outbe-chain/OCOMP/CE files. Recipient ordinary file placement is sufficient to use those stored databases; no conversion, replay from genesis or snapshot file placement service.

## Dependencies and delivery

Depends on: 01.

First independently usable PR: tasks 01+02.

## Exact file changes

| File | Action | Responsibility |
|---|---|---|
| `crates/blockchain/snapshot/src/archive.rs` | new | write_archive / read_archive_index: versioned manifest-first regular tar with mandatory signature then exact declared payloads; shared reader used by optional validation; payloads are ready native files. |
| `crates/blockchain/snapshot/src/create.rs` | new | create_snapshot: inventory and digest pass, required signing, streaming second pass into an exclusive pending file, source-change check and durable final publication. |
| `crates/blockchain/snapshot/src/provenance.rs` | new | Domain-separated manifest digest, signature envelope and expected-key verification using existing secp256k1 format; signature is independent of semantic validity. |
| `crates/blockchain/snapshot/src/fs.rs` | new | Contained file access, no-follow enumeration, same-directory pending publication/fsync and scoped cleanup; no host runtime lease namespace. |
| `crates/blockchain/snapshot/tests/create.rs` | new | Streaming creation, moved archive, source mutation, failure cleanup and nonempty data round trip. |
| `crates/blockchain/snapshot/tests/provenance.rs` | new | Signed success, missing-signature rejection, expected/wrong key, altered raw manifest/signature/payload and signing failure. |
| `bin/outbe-chain/src/cli/snapshot/mod.rs` | new | Snapshot parser/dispatch; only implemented commands registered. Later task additions are registration-only. |
| `bin/outbe-chain/src/cli/snapshot/create.rs` | new | Create args and output; required explicit signing-key loader; forward trailing ordinary node options to offline config resolver. |
| `bin/outbe-chain/src/snapshot/create.rs` | new | Compose native inspection, resolved inventory, OutbeEvmSigner and pure archive writer. |
| `bin/outbe-chain/tests/snapshot_create.rs` | new | Real process CLI create against native stopped fixture; verify no listeners/consensus/OCOMP process appears. |
| `bin/outbe-chain/src/main.rs` | administrative | Intercept snapshot next to dkg/tee/ocomp before run_node; register snapshot module, no change to ordinary node path. |
| `bin/outbe-chain/src/cli/mod.rs` | administrative | Register snapshot CLI module. |
| `crates/blockchain/snapshot/src/lib.rs` | administrative | Register create/archive/provenance/fs exports. |
| `crates/blockchain/snapshot/Cargo.toml` | administrative | Add existing crypto pins only if not already declared; no network or runtime dependencies. |
| `bin/outbe-chain/src/snapshot/mod.rs` | administrative | Register create adapter; same offline-only module. |

## Implementation

1. CLI: snapshot create --output FILE --signing-key FILE [--creator LABEL] [--source LABEL] -- <ordinary node arguments>. Output must be outside all source roots and absent; creation does not start/restart the donor. Optional validator-status metadata is explicitly a signed observation/claim with its reference height/hash and stated basis; it must not be presented as independently verified membership merely because a signature verifies.
2. Enumerate/hash regular data files in deterministic domain/path order. Reject symlink/hardlink/special entries and source/output aliases; open relative to anchored directory handles. Do not recursively copy configuration or all of consensus/OCOMP root.
3. Manifest bytes are fixed serialized struct/ordered lists. Bundle ID = SHA256(raw manifest bytes). Signing digest = SHA256(b"outbe/snapshot/manifest/v1\0" || bundle_id). Envelope contains version, scheme, bundle_id, compressed public key and 65-byte recoverable low-S secp256k1 signature. Use existing OutbeEvmSigner and recovery primitives; no new key custody.
4. Signing is mandatory and must succeed before successful publication. Verify the emitted signature and key, then stream manifest/signature/payload directly from stopped files to a sibling pending archive. Rehash streamed payload and compare with the manifest before final rename. No complete intermediate native data copy.
5. Durably flush pending archive, rename only to an absent final target, fsync parent. If durability fails after rename, report the precise published-but-durability-uncertain outcome, not a nonexistent file. Cleanup only this operation's unpublished temporary files; never delete source data.
6. Archive is self-contained and has no absolute donor path dependency. Normal node ignores its metadata. File format/parser resource limits protect malformed inputs but impose no promised snapshot size/time SLA.
7. Creation attests exact raw manifest bytes, signed file inventory and recorded observations; it does not claim optional semantic checks passed. Keep raw manifest/signature portable after donor relocation; never verify a reserialized replacement. Lawful GC and unequal Q/P are not reasons to add a semantic gate to creation.
8. Archive payloads are ordinary files/directories already in native formats. A tar is only a transport container: conventional extraction/copy places bytes according to documented domain mapping. Do not generate logical table exports requiring import, rebuilding indexes/tries or recalculating state before startup. Metadata/signature stay separate from native stores. Creation accepts stopped unfinished OCOMP stages without requiring Lysis completion.

## Tests first

1. First behavioral RED: actual stopped fixture create returns a readable artifact with every declared nonempty domain; donor unavailable afterwards.
2. Missing signing key, absent signature or signing failure must never produce a successful snapshot; there is no unsigned creation mode. Wrong expected key, tampered H/network/date/digest or payload fails the relevant check.
3. Source file replacement/truncation/change between inventory and streaming fails without final success; no full temporary data directory is created.
4. CLI returns useful artifact/H/hash/signature status and leaves source/config/keys unchanged; ordinary node invocation retains identical dispatch.
5. Required-signature CLI regression: omitting --signing-key fails before publication; signature envelope identifies the signing public key; changing any signed metadata or file digest invalidates verification. No unsigned fallback option exists.
6. Streaming creation with stable unequal Q/P and lawful GC inventory signs the recorded cut without normalization. Relocate artifact and verify raw manifest signature with embedded public key and no producer secret; parsed/reserialized JSON is not substituted for signed bytes.
7. Stopped incomplete OCOMP fixture creates a signed artifact without invoking compute or demanding terminal results. Standard archive extraction exposes unchanged native file bytes and no reconstruction recipe; file handling does not iterate blocks or execute EVM.

## Definition of done

1. A usable create command exists; artifact is portable, complete and signed using current primitives.
2. No live capture, payload wrapper, startup/shutdown modification or producer dependence on a receiver/full semantic validator.
3. fmt, package tests and zero-warning Clippy pass; nonempty native CLI fixture succeeds, and no external dependency identities change.

## Execution discipline

Write the listed failing behavioral unit/fixture tests first, implement the complete slice, then run its integration checks. Task 09 owns real process E2E. Do not count source review or tests from the discarded branch as execution evidence. Keep live progress in Beads.
