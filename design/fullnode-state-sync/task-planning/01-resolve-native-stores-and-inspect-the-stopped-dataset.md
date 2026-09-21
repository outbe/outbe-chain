# 01. Resolve native stores and inspect the stopped dataset

Beads: `outbe-chain-g14b.15`. Baseline: `e5d545cedc872c0e579fd3535e7638d883d5b1f5`.

## Outcome

Resolve ordinary native paths and observe durable progress without starting or mutating data. Define the exact ready-native-file inventory for snapshot creation, optional validation and operator file placement.

## Dependencies and delivery

Depends on: none.

Part of the first create PR together with task 02; do not ship an unused foundation-only feature.

## Exact file changes

| File | Action | Responsibility |
|---|---|---|
| `bin/outbe-chain/src/snapshot/mod.rs` | new | Offline module registration only; no runtime service. |
| `bin/outbe-chain/src/snapshot/config.rs` | new | parse_node_inputs / resolve_layout: parse normal node arguments through Cli<OutbeChainSpecParser, ConsensusArgs, OutbeRpcModuleValidator>; derive D/S/O/P, configured external roots and protected paths. |
| `bin/outbe-chain/src/snapshot/native.rs` | new | inspect_stopped_stores: read Reth durable finalized header/hash, CE marker, projection state and native OCOMP closure using read-only APIs; preserve distinct observed frontiers. |
| `bin/outbe-chain/src/snapshot/inventory.rs` | new | Build a finite domain allowlist and protected-path set from native layout; enumerate the complete required public stores and relevant OCOMP records. |
| `crates/blockchain/snapshot/Cargo.toml` | new | Small outbe-snapshot library for file format and offline operations; use already pinned serde/serde_json/sha2/tar/hex dependencies. No engine/runtime dependency. |
| `crates/blockchain/snapshot/src/lib.rs` | new | Export only offline schema/layout APIs needed by create/validate. |
| `crates/blockchain/snapshot/src/manifest.rs` | new | SnapshotManifestV1, NativeProgress, DomainInventory, FileEntry and format validation; finite logical domains, exact raw manifest digest and preserved declared member order. Do not impose path-location or sorted-order policy on manifest values. |
| `crates/blockchain/snapshot/src/layout.rs` | new | ResolvedDomain/ProtectedPaths and disjoint-path checks; reject overlap with config/key/signing-safety paths, output/staging or another domain. |
| `crates/blockchain/snapshot/tests/layout.rs` | new | Native domain layout, alias/protected overlap and malformed schema tests. |
| `bin/outbe-chain/src/snapshot/tests/layout.rs` | new | Actual native CLI defaults/overrides, external offchain TOML-relative path resolution and source-preservation fixtures. |
| `bin/outbe-chain/src/snapshot/tests/native.rs` | new | Nonzero real-store fixture; preserve H/C/projection distinctions, missing native cursor and wrong-chain failures, read-only source fingerprint. |
| `Cargo.toml` | administrative | Register outbe-snapshot workspace member/dependency and only existing pinned library dependencies. |
| `bin/outbe-chain/Cargo.toml` | administrative | Add outbe-snapshot and the required already-pinned native read-only/storage APIs. |
| `Cargo.lock` | administrative | Resolve the new local package without external dependency version/source/checksum changes. |
| `bin/outbe-ocomp/src/discovery_spool.rs` | modify | Add inspect_closure_checkpoint(path, expected_baseline) beside the current checkpoint decoder; open/read existing bytes only, no directory/lock/checkpoint creation or recovery. |
| `bin/outbe-ocomp/tests/discovery_spool.rs` | modify | Extend existing nonzero closure tests: read-only inspect returns native baseline/previous/current; missing/corrupt/truncated/wrong-baseline input fails without creating or recovering any file. |
| `bin/outbe-chain/src/snapshot/tests/mod.rs` | new | Register layout and native fixtures; used by later validation tests without duplicating store constructors. |

## Implementation

1. Reuse the existing CLI parser only: synthesize argv [outbe-chain,node,...native args], extract Commands::Node and DatadirArgs::resolve_datadir. Never call Cli::run/configure, run_node, CRS initialization or TEE provisioning. Native arguments follow -- after snapshot command options; a Reth TOML alone is not a unified Outbe configuration.
2. D is resolved chain datadir; S is configured consensus directory or D/consensus; O is parent(D)/ocomp/domain-v1; P is the RocksDB primary in projection.storage-config, resolved relative to that TOML. Honor separate static-files and execution-rocksdb paths. Reject Mongo for this filesystem-only release.
3. Copy Reth db/static_files/execution rocksdb, CE compressed_entities/smt, public Marshal prefix archives/cache/application-metadata, finalized_parent_certs, OCOMP retention and the full offchain primary. Copy the native OCOMP closure/spools, installed bundles, CAS objects and linked receipts/bindings/input references/admissions/results/public pending-action data. Use the accompanying source inventory for exact paths, not a blanket copy of the parent directory. Exact paths and protected identities are frozen in design/fullnode-state-sync/data-layout.md; include fatal evidence and required nonsecret catalog lock placeholders. No worker replay inbox or uncommitted CAS staging is accepted as a completed public result.
4. Exclude donor P2P/JWT/private keys, configured DKG key material, sealed enclave identity, sign-once and signed vote/materialization/payout transaction journals. Preserve existing recipient versions. Refuse a layout in which selected database roots overlap a protected file; never silently discard required data to make such a layout fit. Explicitly exclude legacy consensus-root dkg_share.hex, pending-share/dealer/player retry files and outbe-simplex-* partitions as well as configured keys_dir. Installation units are individual public subroots/partitions, never whole S or O parents containing protected siblings.
5. Use Reth open_db_read_only, native static-file read-only readers, CeMdbxReadOnly and Outbe RocksDbReader with external scratch. Do not invoke constructors that initialize missing closure/retention/parent-certificate state. Reuse existing inspect_retention_journal in node/ocomp/retention/inspection.rs. Add a nonmutating closure-checkpoint inspector in the existing discovery_spool codec owner; no duplicated private binary formats. Full body/catalog validation is a later task and is not a dependency of this cheap inspection. Native reader bookkeeping in existing lock files is not authoritative-state mutation; test source database contents and membership separately from documented library lock behavior.
6. Read H from Reth ChainState key LastFinalizedBlock. Read Execution/Finish, current state tip, CE marker Q, projection P and OCOMP C separately. The native data may contain a tail above H; preserve it without truncation or synthetic cursor equality. Later full validation checks each domain at its actual recorded height and the retained canonical links, not latest state against H by assumption. Decode Metadata(storage_settings) strictly: absent means supported legacy v1; malformed is an error, never silently legacy. Cheap creation inspection is not the optional full state/OCOMP audit. Observe partial_state_trie/unwind markers as well as Finish; Finish alone does not prove a complete raw execution state. Preserve native markers and report them; never relabel mixed tables as a complete state at a convenient height.
7. The operator must stop outbe-chain, OCOMP and CE writers, including embedded tasks, before creation and keep them stopped during copying. Lysis completion is not a condition checked by creation. Prefer a quiet point outside OCOMP computation as an operator recommendation only. Preserve unfinished native progress/data as found; no wait-for-Lysis, live request, runtime lock/socket or complete-work gate. Detect unsafe files and observed mutations without claiming read-only opens prove collective shutdown.
8. Freeze the format before downstream work: version, chain/genesis, finalized H/hash, actual progress observations, producer/time/optional source attestation, explicit domain inventory, recorded file paths/sizes/SHA256, required empty-domain records. No host absolute path grants extraction authority.
9. OCOMP fatal evidence is a finite domain O/node-v1/fatal-evidence, sibling of exex-checkpoint. Include native sticky/mismatch/fatal-local evidence unchanged; file placement must not erase recipient evidence or imply a failed node becomes healthy. No automatic merge/repair tool is part of this feature.
10. Public materialization references use the real nested layout O/supervisor-v1/materialization-references/<job>/<first_nod_ordinal>/<job>.materialization-refs-v1.json. Copy the public subtree including all ordinal directories and owner-valid pending records; the job appears in both directory and filename. A flat job-file enumerator is incorrect.
11. The manifest and operator layout list each packaged domain path and its ordinary target root/config argument. Payload files retain native formats usable in place; metadata describes their location, not a recipe to rebuild DB rows, roots, indexes or jobs.
12. User clarification2026-09-20: validate manifest format and data consistency, retain required signature and actual-file checksum verification. Do not add restrictions on declared path location/spelling or require sorted members. Do not replace this clarification with removal of all format validation. Operator controls file placement; no extraction service is introduced.

## Tests first

1. Unit first: native defaults/overrides resolve identically to current launch; relative RocksDB paths are relative to TOML; symlink aliases, overlapping domains/output and keys nested inside selected roots fail.
2. Unit first: manifest format rejects unsupported versions/data classes, duplicate fields or entries, inconsistent counts and malformed required hashes. Differing H/C/projection fields, declared paths and entry order round-trip unchanged. No absolute/parent-path rejection or sorted-member requirement; exact raw bytes remain the signature input.
3. Real native fixture: nonzero chain/CE/projection/closure and public archives, optional configured external roots; missing closure must be reported, never initialized; source persistent bytes unchanged after success/error.
4. Source stop remains a documented operator precondition; no test falsely asserts a collective process lock exists. Observed file changes during enumeration/copy cause failure.
5. Real fixture E=H+1 with differing CE/projection/OCOMP markers: retain the full tail and observed values; do not stamp or compare latest state root to H. Corrupt storage_settings must fail, absent legacy settings must select v1 explicitly.
6. Nonempty nested materialization records at multiple job/ordinal paths survive exact inventory/copy. Present, retired and GcPending native records are preserved as found, not normalized; cheap inspection makes no all-semantic-checks-passed claim.
7. Native inventory covers lawful empty/retired populations as well as nonempty domain fixtures; preserve partial-state markers. Every-domain fixture coverage is not a production prohibition on legal emptiness.
8. A stopped native fixture containing unfinished OCOMP work can be inventoried and signed. Creation has no Lysis-completed check and performs no job execution, progress rewrite, state rebuild or wait for a deadline.

## Definition of done

1. Every native reader domain and protected identity class has an explicit include/exclude row and fixture/sentinel coverage, including legal empty/retired cases. Known missing required domains fail inspection.
2. Native parsing/opening is demonstrated on the pinned main APIs without launching services or source recovery writes; unsupported readers are not silently treated as valid.
3. Manifest/layout are fixed for tasks 02–06; test/caller consumers compile; no reference to removed state-sync/capture/bootstrap modules.
4. All specified native public domains and existing linked records are included faithfully, with exact frontiers and explicit absent/unsupported observations. Complete artifact publication proves inventory/readback, not that optional semantic checks or every startup prerequisite already passed. Tasks06/08/09 separately establish current-obligation coverage and ordinary continuation.

## Execution discipline

Write the listed failing behavioral unit/fixture tests first, implement the complete slice, then run its integration checks. Task 09 owns real process E2E. Do not count source review or tests from the discarded branch as execution evidence. Keep live progress in Beads.
