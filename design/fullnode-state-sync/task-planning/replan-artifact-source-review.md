# Offline commands: current-main evidence

Baseline `e5d545ce`. Tier2 graph coverage was stale/excluded for bin and several
split modules; current source and pinned dependency source were checked directly.
No Cargo/build/test result is claimed. Proposed new APIs appear in task file maps.

## CLI and storage seams

`bin/outbe-chain/src/main.rs` dispatches dkg/tee/ocomp before run_node; snapshot
uses that same offline dispatch point. Parse the ordinary NodeCommand to resolve
configured paths, without Cli::run/configure, launch, CRS or TEE initialization.
A Reth TOML alone does not contain all Outbe identity/storage arguments.

Pinned Reth v2.5.2 commit5a6940e has RO MDBX and static-file readers. Its read-only
RocksDBBuilder creates a sibling secondary under the source parent; do not use
that builder for source inspection. Header/state audit uses MDBX/static files.
Outbe RocksDbReader accepts explicit external scratch. No reader lock proves
collective node/OCOMP shutdown; operator stop remains the precondition.

Use native CeMdbxReadOnly and existing inspect_retention_journal. Closure,
admissions, export bindings and local-result openers may create/recover records;
add narrow observational methods in their format owners instead of calling open.
Existing read-only input-ref catalogs require their nonsecret lock placeholder.

## Validation algorithms and real API routes

- Reth StorageSettings absent selects legacyv1; malformed present JSON fails.
  v1 authoritative plain account/storage is hashed into fresh scratch; v2 uses
  authoritative hashed tables. Rebuild empty scratch tries and check referenced
  bytecode against complete actual E header. Preserve E>H tail and observe partial
  persistence/unwind metadata. Base OCOMP relations use current-E immutable jobs
  plus retained headers; historical scratch rewind is removed from scope.
- Direct scratch hashed-slot lookup implements public primitives
  storage::readonly::StorageReader. Exact duplicate-key match is required.
  ReadOnlyStorageProvider::new_with_block_context exposes existing public
  Metadosis job/generation/terminal, Intex contributor and Nod generation getters.
  No Reth StateProviderFactory, EVM execution or contract-schema export is needed.
  Task04 adds one observational Metadosis live-job view behind the existing public
  api.rs facade; task06 uses native NOD FIFO and Intex series/round/bitmap indexes
  for independent obligation discovery. No new ABI/transition/storage change.
- CE exhaustive audit is new. Enumerate every row, use native codecs and Poseidon
  store traits, compare rebuilt branches/roots in both directions. Materialized
  collections have exactly K shard-root rows including zero roots; absent/retired
  collections have neither catalog entry nor residual rows. Empty catalog has an
  explicit ZERO root row; native sealed_root(ZERO) is the genesis commitment.
- Primary bodies need complete reader scans; indexes cannot prove their own
  completeness. Compare live bodies and CE only at matching actual frontiers.
  Retained Tribute bodies have lifecycle-specific obligations: GcPending can be
  partial/empty; Released/pruned history is not required. A live partition binds
  to JobIntent collection root/count, not only the catalog-wide CE root.
- AdmissionCatalogReader must bridge to LocalLysisPlanAuditV1's existing concrete
  catalog reference using a crate-private immutable view; a new unrelated reader
  cannot be passed to the current open function unchanged.
- Materialization references are public CAS retention declarations, not cursors
  or transaction authority. Verify membership in the same plan/result, not
  equality to a later canonical cursor's newly constructed batch.
- contributor-payout-v1.bin contains raw fixed84-byte contributor leaves. Verify
  count/root/checked nominal sum with existing Intex primitives and canonical
  active job/generation; do not invent a wrapper or invoke the submitter.

Signatures use the existing OutbeEvmSigner/crypto primitives. The new manifest
format fixes domain-separated signing bytes and expected-key checks in task02.
Signed source/status metadata is the signer's attestation, not automatically an
independent proof of membership or computation.

The removed state-sync/capture/VerifiedImage/adoption helpers do not exist on
main and are not reuse points. All new offline files are identified as new.
No vendor patches, new dependency identities, or current-main runtime recovery
changes are needed to express the planned offline adapters. Exact generic/import
compilation and behavior remain implementation tests, not completed evidence.

The subsequent [input-sufficiency audit](../validation-audit.md) supplies the corrected nested materialization path, GC rules, current-state bindings and independent obligation coverage. These corrections are part of current task bodies, not a request to add a startup subsystem.
