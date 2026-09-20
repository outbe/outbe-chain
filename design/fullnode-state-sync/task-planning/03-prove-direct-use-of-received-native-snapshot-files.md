# 03. Prove direct use of received native snapshot files

Beads: `outbe-chain-g14b.17`. Baseline: `e5d545cedc872c0e579fd3535e7638d883d5b1f5`.

## Outcome

Demonstrate that received snapshot files can be placed at ordinary configured paths and used directly. This task adds file-layout documentation and acceptance tests, not a restore/install command or reconstruction subsystem.

## Dependencies and delivery

Depends on: 02.

File portability acceptance with the create delivery (tasks01+02+03). This is test/document work, not a second recipient runtime phase.

## Exact file changes

| File | Action | Responsibility |
|---|---|---|
| `bin/outbe-chain/tests/snapshot_files.rs` | new | Create signed native fixture, use conventional extraction/file-copy test glue into independent native roots, compare bytes and use ordinary read-only native openers. No product restore command/helper. |
| `design/fullnode-state-sync/data-layout.md` | modify | Publish payload-domain to ordinary target-root mapping, native format/mode expectations and separation of recipient identity/configuration. |
| `design/fullnode-state-sync/operator-workflow.md` | modify | Document stop -> create -> transfer -> place ready files -> optional validate -> ordinary start; explain tar extraction is byte handling, not state reconstruction. |

## Implementation

1. Use the actual create CLI on nonzero native fixtures. Copy/transfer the resulting artifact to an unrelated location. If packed as tar, extract regular payload members with the conventional tar/file-copy route used in the operator instructions; test glue is not exported as a product installer.
2. Document each logical payload domain and its ordinary destination derived from native configuration. Place the files at those destinations with writers stopped. Preserve content, native layout and required modes; provision recipient identity/config independently. There is no mandatory database import/conversion or additional snapshot-specific command before start.
3. Keep manifest/signature exactly as received if the operator wants file/provenance validation. They are portable sidecar evidence, not startup input. Conventional file transfer/extraction and existing permission setup may scale with byte count; no iteration/reexecution of blocks, rebuilding tables/indexes/state/CE, materializing jobs or redoing Lysis is part of placement.
4. For existing servers, operator file placement must not mix incompatible DB contents or overwrite own keys/config/signing-safety/fatal evidence. Document public-domain boundaries; this feature does not implement --force, staged swaps, rollback/resume journals, automatic evidence union or a multi-filesystem installer. New-node acceptance uses its own provisioned identity and separate native data roots.
5. Verify native stores open from their new ordinary paths after file placement; full ordinary process continuation belongs to tasks08/09. No source donor access, original absolute paths, archive, sidecar or validation receipt is consumed by node startup.

## Tests first

1. RED first: nonzero ready-native fixture packaged by create and conventionally extracted/copied to separate roots is byte-identical before any native readers run. Open MDBX/static-file/CE/projection/OCOMP domains using their ordinary configured paths.
2. Nested NOD job/ordinal paths, required empty directories, native lock placeholders and separate-root domains survive transfer/extraction. The test must fail if it needs a new snapshot-specific converter or generated cursor.
3. Original manifest/signature verify after transfer without donor/private key. Native-only checks and ordinary startup require no metadata; explicitly requested artifact checks name missing sidecars.
4. Recipient keys/config/sign-once/signed-journal sentinels remain outside copy targets. Native local-result owner/mode requirements are met by ordinary recipient filesystem setup, never importing donor UID or introducing service users.
5. Partial-transfer fixture fails requested file inventory validation and is never called complete placement. No restore/resume/rollback helper or claim of atomic multi-root transfer is introduced.

## Definition of done

1. Ready native payload files are usable after ordinary transfer/extraction/copy, with exact source/destination mapping and no required snapshot command between receipt and optional validation/start.
2. File placement performs byte handling only: no EVM replay, root/index/database reconstruction, OCOMP job recovery algorithm or data-format conversion.
3. Tests prove byte preservation and ordinary native opens; task08/09 prove ordinary continuation. No production restore/install API, CLI or journal is introduced.

## Execution discipline

Write the listed failing behavioral unit/fixture tests first, implement the complete slice, then run its integration checks. Task 09 owns real process E2E. Do not count source review or tests from the discarded branch as execution evidence. Keep live progress in Beads.
