# ADR-B-GEN-001: Genesis is a reproducible chain-identity and schema activation manifest

- **Status:** Proposed; current implementation profiled
- **Date:** 2026-07-17
- **Decision owners:** Blockchain Space, release and protocol-schema maintainers
- **Scope:** chain spec parsing, genesis construction/seeding, predeploy artifacts and startup validation
- **Depends on:** ADR-B-WIR-001, ADR-B-CNS-003, ADR-B-EVM-003,
  ADR-B-CLI-001, ADR-S-VAL-001
- **Related:** ADR-B-CRY-001, ADR-B-OCD-007, ADR-B-OCD-008, ADR-B-OCD-010

## Context

An Outbe network is defined by more than EVM `chainId`. Genesis JSON selects the
header/fork schedule, timestamp, validator committee, protocol configuration,
prefunded balances, native-precompile marker accounts, initial module storage, TEE
policy and ordinary EVM predeploy bytecode. Consensus bootstrap separately reads
validator and epoch configuration, while local CE/Mongo stores bind to genesis hash.

Today much of this state is authored by `scripts/prepare_network.py` and a large
handwritten `scripts/seed_genesis.py` that duplicates Rust addresses, slots,
constants and encodings. The executor validates a useful ValidatorSet/Staking subset
at early blocks, but it does not prove the full generated genesis matches the binary.

This ADR owns genesis identity, reproducible construction and activation readiness.
It does not own each module's domain invariants or post-genesis migrations.

## Decision

### Canonical chain identity

Define `ChainIdentityV1` as the digest-bound tuple of:

- canonical genesis block hash and EVM chain id;
- canonicalized genesis specification bytes or an unambiguous manifest digest;
- active header/fork and Outbe protocol-schedule versions at height zero;
- genesis allocation/state root;
- initial consensus committee/epoch parameters and consensus namespace identity;
- protocol address registry and native-precompile manifest root;
- module storage-layout/schema manifest root;
- ordinary predeploy artifact manifest root;
- CE commitment/topology/local-storage identity; and
- required TEE/circuit/cryptographic artifact policy roots.

Runtime components receive this typed identity rather than independently selecting a
chain id, genesis hash or implicit mainnet default. Every persisted store, sealed
secret and cross-process sidecar binds the minimum relevant identity fields and fails
closed on mismatch.

### Reproducible genesis compiler

Replace handwritten cross-language constants with a deterministic genesis compiler
driven by generated manifests from ADR-B-WIR-001, ADR-B-CRY-001 and ADR-B-TST-001 and module-owned typed genesis
schemas. Inputs are explicit, versioned and validated: chain/fork/timing profile,
validator public identities, initial balances/business records, TEE policy and pinned
external artifacts. Secret validator/DKG/EVM keys are never valid genesis inputs.

The compiler:

1. validates all inputs and cross-module preconditions before producing output;
2. derives every address, slot, codec and marker rule from versioned manifests;
3. emits canonical JSON plus a machine-readable provenance/identity report;
4. independently re-decodes the result using production Rust codecs;
5. executes block-0/block-1 validation and state-root construction offline; and
6. is reproducible byte-for-byte from pinned source and public inputs.

No current date, environment-dependent fallback, network fetch or unordered map may
silently change production genesis. Localnet convenience may supply explicit “today”
as an input and records it in the manifest.

### Genesis allocation and predeploy rules

Every protocol address has one declared genesis role: stateful native precompile,
stateless precompile, system-only account, balance accumulator, ordinary bytecode
predeploy or intentionally absent/reserved. The manifest declares code marker,
initial balance, occupied slots/schema version and first authorized writer.

Marker bytecode is an EIP-161 persistence mechanism, not dispatch authority. The
runtime registry and genesis marker set are generated from the same address manifest.
An ordinary predeploy binds address, runtime bytecode hash, storage root, compiler/
source provenance and immutable-address assumptions. Genesis cannot overwrite a
non-identical existing allocation entry.

### Startup validation and readiness

Before proposing, validating or serving authoritative reads, a node validates:

- parsed chain spec and calculated genesis hash/identity;
- DB genesis/canonical head compatibility;
- every mandatory native module account, schema marker and genesis invariant;
- exact ValidatorSet/Staking/Rewards/committee/bootstrap consistency;
- protocol schedule/fork compatibility with the binary;
- CE/Mongo/sidecar identity binding; and
- external artifact roots required by enabled production roles.

Validation is one typed, exhaustive readiness report and occurs before background
actors mutate state. Missing or mismatched mandatory state is startup-fatal; the
executor never opportunistically backfills genesis. Optional dev profiles are
explicit chain profiles and cannot be mistaken for production.

### Schema activation and migration

Genesis selects one layout/codec version for every state owner. Slot zero is not
assumed universally to be a version unless the module manifest declares it. The
genesis compiler and runtime both consume the same generated layout identity.

Post-genesis changes use ADR-S-GOV-003 activation plus a module-owned migration; editing
the seeder or genesis file never migrates an existing chain. A changed genesis creates
a different chain identity and requires fresh stores or an explicitly verified import
procedure.

### Evidence

CI constructs representative production and localnet genesis files from scratch,
runs production Rust parsing/validation, executes genesis plus bootstrap blocks, and
compares state/header/artifact roots across independent nodes. Golden manifests cover
every protocol address and occupied slot. Negative tests mutate every identity field,
module slot, validator key/stake, artifact and policy root and require fail-closed
startup.

## Authoritative interfaces

| Responsibility | Authority |
|---|---|
| Chain identity tuple/digest | `ChainIdentityV1` manifest |
| Address/precompile roles | ADR-B-WIR-001 generated registry |
| Module slots/schema/genesis codec | ADR-B-EVM-003 plus owner module schema |
| Public genesis input compilation | deterministic genesis compiler |
| Consensus bootstrap expectation | typed chain-spec committee profile |
| Runtime genesis readiness | node startup validator |
| Post-genesis activation/migration | ADR-S-GOV-003 and owner ADR |

## Invariants

- Equal public inputs and tool version produce byte-identical genesis and identity.
- A different genesis allocation, committee, policy, artifact or schedule produces a
  different chain identity.
- No secret key/share is present in genesis or its provenance report.
- Every active stateful native precompile survives EIP-161 and has declared schema.
- No address/slot/constant is independently retyped by the seeder.
- Consensus bootstrap and on-chain validator/stake state agree exactly.
- Block 1 is the first writer of fields explicitly reserved for bootstrap.
- An incompatible existing DB/store/sidecar never starts under another identity.
- Genesis validation is read-only, exhaustive and happens before service readiness.

## Atomicity, replay and failure

Genesis compilation writes to a new output and publishes it only after complete
validation; failure leaves no partially trusted artifact. Operator distribution
verifies the identity digest after transfer. Node initialization opens and validates
all stores before actors start; partial store creation is quarantined/removed by the
recovery contract in ADR-B-OCD-007, ADR-B-OCD-014 and ADR-B-OCD-015.

Re-running the compiler is idempotent for identical inputs. Replaying genesis against
an initialized incompatible database fails rather than merging allocation. Block-1
bootstrap is separately replay-protected by ADR-B-CNS-003 and the owning System ADRs.

## Compatibility and migration

Genesis is immutable chain identity. Tool formatting may change only if canonical
identity bytes/digest remain defined independently; any semantic allocation/config
change creates a new genesis hash/network. Layout or protocol evolution after height
zero uses scheduled migrations and retains historical re-execution codecs.

## Production-interface verification evidence

Inspected Reth chain parser wiring, `prepare_network.py`, the Python seeder and its
duplicated registries/layouts, consensus `GenesisValidators` transport, executor
`validate_genesis_state`, block-0/1 handling, CE environment identity, genesis Rust
integration tests, ordinary predeploy artifact tests and localnet launch scripts.
Current evidence covers selected accounts and validator/stake closure but not one
whole-manifest identity/readiness contract. Status remains Proposed.

## Consequences

Genesis becomes a compiled, reviewable protocol artifact rather than a mutable JSON
patching convention. Operators can prove two nodes target the same full network, and
module audits can trace every initial state word to a module-owned schema.

## Rejected alternatives

- **Use only `chainId`:** unrelated genesis states can share it.
- **Treat Python seeder constants as independent authority:** they already drift from
  Rust schemas/tests.
- **Let executor backfill missing genesis state:** proposal/re-execution paths can
  diverge and hide malformed releases.
- **Validate only validator state:** other module schemas/policies can still be
  incompatible.
- **Fetch predeploy/CRS artifacts during genesis build without pins:** output ceases to
  be reproducible or reviewable.

## Open questions and technical debt

1. **Critical:** `COMPRESSED_ENTITIES_SCHEMA_VERSION` in `seed_genesis.py` is `3`,
   while `crates/blockchain/evm/tests/genesis.rs` asserts slot 0 equals `2`; the
   seeder docstring also says V2. Reconcile against the Rust runtime schema and make
   one generated value authoritative.
2. `seed_genesis.py` duplicates a large address list, marker set, schema constants,
   slot arithmetic and economic defaults from Rust. Generate a versioned manifest or
   call production codecs instead of maintaining two implementations.
3. The seeder's `ALL_PRECOMPILE_ADDRESSES` is visibly incomplete relative to the EVM
   registry (for example several newer factory/system addresses). Prove every active
   stateful address is preserved and classify intentionally runtime-only entries.
4. Current tests enumerate registered precompiles using the duplicated registry that
   ADR-B-EVM-001 already found inconsistent. A test derived from two drifting lists is
   not whole-registry evidence.
5. `OutbeChainSpecParser` delegates to the generic Ethereum parser and maps the header.
   Publish which custom `config` fields survive parsing and reject unknown/misspelled
   consensus-critical keys rather than silently defaulting.
6. Many genesis settings default in Python and/or Rust when absent. Production
   profiles must require explicit values and prove both sides use the same source.
7. `parse_genesis_timestamp` falls back to current UTC when `config.genesisTime` is
   absent. A production genesis compiler must never depend on build wall clock.
8. Localnet `--worldwide-day` retargeting mutates multiple business inputs to avoid a
   wedge. Make the day an explicit coherent profile input and validate all dependent
   fields rather than mutating an arbitrary seed in place.
9. The same script seeds deep business state for many Core modules using handwritten
   slots. Add owner-provided typed genesis builders and structural invariant checks
   for every ledger/index/counter, not only serialization.
10. `validate_genesis_state` checks ValidatorSet and Staking but not Rewards anchor,
    governance authorities, Oracle, Cycle, emission, CE schema/root, ZeroFee, TEE
    policy, Core indexes or ordinary predeploy state.
11. Genesis validation is invoked through early block hooks only when a local
    `GenesisValidators` option is supplied. Prove every validator/follower bootstrap
    path supplies it and that validation cannot be skipped on sync/import.
12. `GenesisValidators` contains address/public key/epoch length but not expected
    stake, genesis hash, chain id or manifest digest. It cannot authenticate the full
    state it is used to validate.
13. Bridge `take_genesis_validators` is consumptive mutable state. Prove proposal,
    validation, retries and concurrent builders cannot consume validation authority
    inconsistently; prefer immutable chain configuration.
14. Runtime consensus chain id is installed through a process-wide first-writer-wins
    initializer. Fail explicitly if a second chain spec differs; tests/tools in one
    process must not inherit stale identity.
15. Define one canonical genesis JSON/manifest encoding. Pretty-print/order changes
    should not accidentally alter identity claims independently of the actual Reth
    genesis block hash.
16. `prepare_network.py`, `bootstrap-testnet.sh`, `run-testnet.sh` and e2e bootstrap
    contain overlapping genesis-generation paths. Consolidate them behind one
    compiler and conformance suite.
17. Some bootstrap shell code patches genesis after the main seeder (for example dev
    felony threshold). Post-validation mutation must be prohibited or force a full
    identity/revalidation pass.
18. Governance authority defaults to validator addresses and VaultProvider defaults
    to validator zero. These are security/economic decisions requiring explicit
    production inputs and owner-ADR approval, not convenience defaults.
19. Absence of `tee_policy` produces a running chain whose enclave measurements are
    unchecked. Production chain profiles must require a nonzero authenticated policy;
    dev mode must be visibly distinct.
20. External contracts are fetched/staged through separate scripts and checked by
    selected artifact tests. Bind source/compiler/runtime-code/storage/immutable
    hashes into the genesis manifest for every predeploy.
21. The ignored `handleOps` predeploy e2e means bytecode/address matching does not
    prove the ordinary predeploy is operational with its expected dependencies.
22. Add negative startup tests for wrong chain id/genesis hash, altered validator
    order/key/stake, schema word, marker, TEE policy, predeploy code/storage and CE
    root across validator, follower and RPC-only roles.
23. Define fresh-store cleanup/quarantine after a failed multi-store startup so a
    later retry cannot treat partially created Mongo/CE/Reth state as initialized.
24. Add a signed release artifact containing genesis hash, chain identity/manifest
    roots, tool revisions and reproducibility command; operators currently distribute
    mutable JSON and several side files without one attestation.
