# Architecture coverage ledger

- **Status:** Living inventory; coverage is not acceptance or implementation proof
- **Generated from:** `cargo metadata --no-deps --format-version 1`, repository manifests,
  deployable source trees and registered entrypoints
- **Last reconciled:** 2026-07-18

## Purpose

This ledger prevents “an ADR mentions the crate” from being mistaken for full coverage.
Every production package or entrypoint maps to the ADR that owns its authority and to
the cross-module PFS where applicable. The owning ADR must still contain reachable
commands, state/FSM, atomicity, replay, deterministic bounds, compatibility, production
evidence and open debt.

`Primary ADR` owns the component's invariant. `Imported contract` means the component is
an adapter/test/vendor implementation whose behavior is constrained by that ADR but does
not create a second authority.

## Rust workspace packages

The current workspace contains 57 Cargo packages.

| Cargo package | Physical scope | Primary ADR(s) | Coverage role |
|---|---|---|---|
| `outbe-chain` | `bin/outbe-chain` | ADR-B-NOD-001, ADR-B-SUP-001, ADR-B-OPS-001 | Process entrypoint/lifecycle/deployment profile |
| `outbe-cli` | `bin/outbe-cli` | ADR-B-CLI-001 | Operator transaction intent |
| `outbe-feeder` | `bin/outbe-feeder` | ADR-S-ORC-002 | External Oracle ingestion entrypoint |
| `outbe-keygen` | `bin/outbe-keygen` | ADR-S-KEY-001 | Validator key ceremony entrypoint |
| `outbe-tee-enclave` | `bin/outbe-tee-enclave` | ADR-S-TEE-001, ADR-S-KEY-001 | Enclave and mock entrypoints |
| `outbe-consensus` | `crates/blockchain/consensus` | ADR-B-CNS-001 through ADR-B-CNS-003, ADR-B-CRY-001 | Consensus/DKG/delivery authority |
| `outbe-engine` | `crates/blockchain/engine` | ADR-B-CNS-003, ADR-B-EVM-001 | Consensus/execution bridge |
| `outbe-evm` | `crates/blockchain/evm` | ADR-B-EVM-001 through ADR-B-EVM-003, ADR-B-OCD-007 | Block execution authority |
| `outbe-macros` | `crates/blockchain/macros` | ADR-B-EVM-004 | Generated storage/ABI contract |
| `outbe-node` | `crates/blockchain/node` | ADR-B-NOD-001, ADR-B-SUP-001, ADR-B-OCD-004 | Node assembly/supervision/projection |
| `outbe-offchain-storage` | `crates/blockchain/offchain-storage` | ADR-B-OCD-001 | Memory/Mongo storage capability |
| `outbe-primitives` | `crates/blockchain/primitives` | ADR-B-WIR-001, ADR-B-EVM-003 and ADR-B-EVM-004 | Protocol types/storage DSL |
| `outbe-rpc` | `crates/blockchain/rpc` | ADR-B-RPC-001, ADR-B-OCD-005 and ADR-B-OCD-013 | RPC read authority |
| `outbe-txpool` | `crates/blockchain/txpool` | ADR-B-TXP-001, ADR-S-FEE-001 | Admission/order adapter |
| `outbe-common` | `crates/core/common` | ADR-B-EVM-005, ADR-C-POW-001, PFS-002 | Shared domain types; no independent ledger |
| `outbe-compressed-entities` | `crates/core/compressed-entities` | ADR-B-OCD-006 through ADR-B-OCD-015 | Authenticated off-chain state authority |
| `outbe-sparse-merkle-tree-v061` | vendored modified SMT | ADR-B-OCD-008, ADR-B-CRY-001 | Imported consensus-critical implementation |
| `outbe-sparse-merkle-tree-v061-pristine` | vendored reference SMT | ADR-B-OCD-008, ADR-B-TST-001 | Differential reference implementation |
| `outbe-tribute` | `crates/core/tribute` | ADR-C-TRB-001 | Tribute ledger |
| `outbe-tributefactory` | `crates/core/tributefactory` | ADR-C-TRB-002, PFS-001 | Encrypted offer admission |
| `outbe-nod` | `crates/core/nod` | ADR-C-NOD-001 | Nod ledger |
| `outbe-nodfactory` | `crates/core/nodfactory` | ADR-C-NOD-002, PFS-002 | Nod issuance/mining orchestration |
| `outbe-gratis` | `crates/core/gratis` | ADR-C-GRT-001 | Gratis ledger |
| `outbe-gratisfactory` | `crates/core/gratisfactory` | ADR-C-GRT-002, PFS-003 | Gratis workflow authority |
| `outbe-metadosis` | `crates/core/metadosis` | ADR-C-MET-001, PFS-002, PFS-004 and PFS-009 | WorldwideDay orchestration |
| `outbe-agentreward` | `crates/core/agentreward` | ADR-C-AGR-001 | Agent reward ledger |
| `outbe-lysis` | `crates/core/lysis` | ADR-C-LYS-001, PFS-002 and PFS-009 | Tribute-to-Nod transformation |
| `outbe-fidelity` | `crates/core/fidelity` | ADR-C-FID-001 | Fidelity cohorts |
| `outbe-promis` | `crates/core/promis` | ADR-C-PRM-001 | Promis ledger |
| `outbe-promisfactory` | `crates/core/promisfactory` | ADR-C-PRM-002, PFS-003 and PFS-004 | Promis conversions |
| `outbe-promislimit` | `crates/core/promislimit` | ADR-C-PRM-003 | Allocation limit |
| `outbe-credis` | `crates/core/credis` | ADR-C-CRD-001 | Credis position FSM |
| `outbe-credisfactory` | `crates/core/credisfactory` | ADR-C-CRD-002, PFS-003 | Credis orchestration |
| `outbe-vaultprovider` | `crates/core/vaultprovider` | ADR-C-VLT-001 | Liquidity authority |
| `outbe-intex` | `crates/core/intex` | ADR-C-INX-001, PFS-009 | Native Intex ledger |
| `outbe-intexfactory` | `crates/core/intexfactory` | ADR-C-INX-002, PFS-004 and PFS-009 | Native Intex orchestration |
| `outbe-gem` | `crates/core/gem` | ADR-C-GEM-001 | Gem ledger |
| `outbe-gemfactory` | `crates/core/gemfactory` | ADR-C-GEM-002 | Gem orchestration |
| `outbe-desis` | `crates/core/desis` | ADR-C-DES-001, PFS-004 and PFS-009 | Native cross-chain auction FSM |
| `outbe-governance` | `crates/core/governance` | ADR-S-GOV-001 | System authority despite physical path |
| `outbe-cycle` | `crates/system/cycle` | ADR-S-CYC-001 | Scheduler |
| `outbe-validatorset` | `crates/system/validatorset` | ADR-S-VAL-001 | Validator registry |
| `outbe-staking` | `crates/system/staking` | ADR-S-STK-001 | Stake ledger |
| `outbe-rewards` | `crates/system/rewards` | ADR-S-RWD-001 | Participation/reward settlement |
| `outbe-slashindicator` | `crates/system/slashindicator` | ADR-S-SLS-001 | Offense/slashing authority |
| `outbe-accounting` | `crates/system/accounting` | ADR-S-ACC-001 | Certified progress |
| `outbe-emissionlimit` | `crates/system/emissionlimit` | ADR-S-EMI-001 | Emission policy |
| `outbe-oracle` | `crates/system/oracle` | ADR-S-ORC-001 | Oracle ledger/tally |
| `outbe-tee` | `crates/system/tee` | ADR-S-TEE-001 | Node/enclave client protocol |
| `outbe-teeregistry` | `crates/system/teeregistry` | ADR-S-TEE-002 | Enclave/key registry |
| `outbe-vote` | `crates/system/vote` | ADR-S-GOV-002 | Vote FSM |
| `outbe-update` | `crates/system/update` | ADR-S-GOV-003 | Protocol activation |
| `outbe-zerofee` | `crates/system/zerofee` | ADR-S-FEE-001 | Fee policy/hooks |
| `outbe-zkproof` | `crates/system/zkproof` | ADR-S-ZKP-001 and ADR-S-ZKP-002 | Verifier/hash profile |
| `outbe-offchain-data` | `crates/system/offchain-data` | ADR-B-OCD-003 through ADR-B-OCD-005 | Projection/runtime readers; Blockchain responsibility |
| `outbe-e2e` | `crates/core/e2e` | ADR-B-TST-001, PFS-002 and PFS-005 | In-process integration evidence, not process E2E |
| `outbe-e2e-harness` | `crates/testing/e2e-harness` | ADR-B-TST-001, PFS-001 and PFS-006 | Process/localnet/Mongo evidence harness |

`crates/blockchain/primitives/fuzz/Cargo.toml` is deliberately outside the workspace;
its fuzz targets are verification evidence for ADR-B-WIR-001 and ADR-B-EVM-003 and must be run by
an explicit CI job rather than silently counted among the 58 packages.

### Executable target and command registry

| Executable surface | Command/effect groups | Owning ADR(s) |
|---|---|---|
| `outbe-chain` | Reth `node` plus Outbe validator/follower/config extensions | ADR-B-NOD-001, ADR-B-OPS-001 and ADR-B-SUP-001 |
| `outbe-chain dkg` | `bootstrap`, `status`, `export-share`, `import-share`, `force-restart` | ADR-B-CNS-002, ADR-S-KEY-001 and ADR-B-CLI-001 |
| `outbe-cli` | `validator`, `staking`, `rewards`, `epoch`, `slash`, `chain`, `monitor`, `oracle`, `tribute`, `zero-fee`, `tee`, `vote` | ADR-B-CLI-001 plus the referenced System/Core owner ADR |
| `outbe-keygen` | `generate`, `show-pubkey`, `sign-registration`, `verify`, `hybrid` | ADR-S-KEY-001 and ADR-B-CLI-001 |
| `outbe-feeder` | external provider polling/aggregation and Oracle delivery | ADR-S-ORC-002 |
| `outbe-tee-enclave` | production enclave transport/service | ADR-S-TEE-001 and ADR-S-KEY-001 |
| `outbe-tee-enclave-mock` | explicitly non-production enclave test service | ADR-S-TEE-001 and ADR-B-TST-001 |
| `outbe-e2e` | process/localnet scenario runner | ADR-B-TST-001, PFS-001 and PFS-006 |

Every command that signs, deletes, imports, resets or publishes state is an operator
mutation even when it bypasses EVM transactions. In particular DKG `force-restart` and
share import/export inherit the confirmation, identity, secret and recovery rules of
ADR-B-CLI-001 and ADR-S-KEY-001.

## Solidity packages and deployable authorities

| Package | Stateful authorities | Primary ADR(s) |
|---|---|---|
| `contracts/precompiles` | Solidity ABI/interface artifacts | ADR-B-EVM-004 and ADR-B-EVM-005 plus each System/Core owner ADR |
| `contracts/crosschain` | ERC-7786 facade, Hyperlane/LayerZero/loopback adapters | ADR-B-XCH-001 |
| `contracts/tokens` | WCOEN/synthetic ledgers; fungible bridge | ADR-C-TOK-001 and ADR-C-TOK-002 |
| `contracts/intex` | ERC-1155 ledger/bridge, target auction/escrow, routers | ADR-C-INX-003 through ADR-C-INX-007, ADR-C-DES-001, PFS-004, PFS-009 |
| `contracts/intent` | Origin/destination order FSM, solver auction/collateral | ADR-C-INT-001 through ADR-C-INT-003 |
| `contracts/smart-account` | Factory, bundle plugin/hooks/policies/signers | ADR-B-SMA-001, PFS-003 |

Deployment scripts, proxy wiring and upgrade drills are governed by ADR-B-DEP-001 and
remain production interfaces of each package owner; they are not “just scripts”. Every
upgradeable package must bind storage layout, immutables, roles, peers, addresses and code
hashes in its activation evidence.

## Non-Cargo runtime and operator surfaces

| Surface | Primary ADR(s) | Required evidence |
|---|---|---|
| `mcp` npm package / `outbe-mcp` | ADR-B-MCP-001 | read/write separation, key safety, transaction saga tests |
| `mise.toml`, `scripts`, Docker/localnet | ADR-B-OPS-001, ADR-B-SUP-001, ADR-B-TST-001 | reproducible topology/readiness/shutdown/cleanup |
| `deploy/systemd`, `deploy/monitoring` | ADR-B-OPS-001, ADR-B-SUP-001 | deployment profile, failure propagation, probes, alerts and secret/config ownership |
| `.github/workflows`, `Dockerfile`, `.goreleaser.yaml` | ADR-B-RLS-001, ADR-B-TST-001 | exact artifact gates, provenance, SBOM, signing and publication |
| `Cargo.lock`, npm locks, Foundry deps, `supply-chain`, `deny.toml`, `audit.toml` | ADR-B-RLS-001 | locked dependency graph, review/advisory/license policy and exceptions |
| genesis/canonicalization scripts | ADR-B-GEN-001, ADR-B-CRY-001 | reproducible manifest/hash verification |
| `benchmarks/adr009` and CE benches | ADR-B-CAP-001, ADR-B-TST-001 | pinned hardware/profile and raw artifacts |
| `e2e/evm` | ADR-B-TST-001 | production-interface classification and CI execution |
| `examples/credis-flow` | ADR-C-CRD-001 and ADR-C-CRD-002, PFS-003 | illustrative only unless promoted to gated evidence |

## Registered execution coverage

Package coverage is insufficient unless these registries are also reconciled:

- every generated/stateful precompile address and ABI selector maps to one System/Core
  owner ADR plus ADR-B-EVM-004 and ADR-B-EVM-005;
- every begin/end block lifecycle hook maps to its module ADR and ADR-B-EVM-001;
- every RPC method maps to ADR-B-RPC-001 and, for authenticated data, ADR-B-OCD-013;
- every binary/subcommand and MCP mutation maps to an intent/safety ADR;
- every persistent database/table/collection/checkpoint maps to one authority and
  recovery ADR;
- every externally asynchronous effect maps to an inbox/outbox state or explicit debt.

### Stateful precompile dispatch registry

The production `outbe_dispatch_fn` registry currently maps as follows. All entries also
import ADR-B-EVM-002, ADR-B-EVM-004 and ADR-B-EVM-005 for call-frame, ABI and runtime
rules; this table names the state/business owner.

| Registered dispatch names | Owning ADR(s) |
|---|---|
| `tribute`, `tributefactory` | ADR-C-TRB-001 and ADR-C-TRB-002 |
| `nod`, `nodfactory` | ADR-C-NOD-001 and ADR-C-NOD-002 |
| `gratis`, `gratisfactory` | ADR-C-GRT-001 and ADR-C-GRT-002 |
| `metadosis`, `agentreward` | ADR-C-MET-001 and ADR-C-AGR-001 |
| `fidelity` | ADR-C-FID-001 |
| `promis`, `promisfactory`, `promislimit` | ADR-C-PRM-001 through ADR-C-PRM-003 |
| `credis`, `credisfactory`, `vaultprovider` | ADR-C-CRD-001, ADR-C-CRD-002 and ADR-C-VLT-001 |
| `intex`, `intexfactory` | ADR-C-INX-001 and ADR-C-INX-002 |
| `gem`, `gemfactory` | ADR-C-GEM-001 and ADR-C-GEM-002 |
| `desis` | ADR-C-DES-001 |
| `validatorset` | ADR-S-VAL-001 |
| `staking` | ADR-S-STK-001 |
| `rewards` | ADR-S-RWD-001 |
| `slashindicator` | ADR-S-SLS-001 |
| `oracle` | ADR-S-ORC-001 |
| `teeregistry` | ADR-S-TEE-002 |
| `governance`, `vote`, `update` | ADR-S-GOV-001 through ADR-S-GOV-003 |
| `zerofee` | ADR-S-FEE-001 |
| `zkproof-poseidon`, `zkproof-groth16` | ADR-S-ZKP-001 and ADR-S-ZKP-002 |
| `outbe-system-tx` | ADR-B-EVM-001 plus the System/Core owner selected by its versioned phase payload |
| `debug-subcall` | ADR-B-EVM-002; production registration remains explicit critical debt there |

Ethereum precompiles `0x01..0x0a` are imported from the selected revm hardfork and are
owned by ADR-B-EVM-002 and ADR-B-GEN-001 compatibility evidence rather than a business ADR.

### Block lifecycle registry

| Effective executor hook | Phase | Owning ADR(s) |
|---|---|---|
| `CompressedEntitiesLifecycle` | block open and final seal | ADR-B-OCD-007 and ADR-B-OCD-012 |
| `VoteLifecycle` | pre-user begin phase | ADR-S-GOV-002 |
| `UpdateLifecycle` | pre-user begin phase | ADR-S-GOV-003 |
| `RewardsLifecycle` | pre-user begin phase | ADR-S-RWD-001 |
| `OracleLifecycle` | pre-user begin phase | ADR-S-ORC-001 |
| `GemLifecycle` | pre-user begin phase | ADR-C-GEM-001 |
| `IntexLifecycle` | pre-user begin phase | ADR-C-INX-002 |
| `NodLifecycle` | cycle-tick system transaction | ADR-C-NOD-001 |
| `CycleLifecycle` | cycle-tick system transaction | ADR-S-CYC-001 |

The authoritative total ordering and receipt-visible system-transaction boundary are
owned by ADR-B-EVM-001. A lifecycle type that exists in a crate but is not reachable from
the production executor is not registered evidence.

### Custom RPC registry

The 13 registered methods are enumerated with consistency class and source in
ADR-B-RPC-001: `outbe_getCompressedEntity`, `outbe_getValidators`,
`outbe_getValidator`, `outbe_getEpochInfo`, `outbe_getStake`, `outbe_getSlashInfo`,
`outbe_consensusStatus`, `outbe_getVrfSeed`, `outbe_getEmissionInfo`,
`outbe_getSlashConfig`, `outbe_getParticipation`, `outbe_syncStatus` and
`outbe_getFinalization`.

### Persistent-store and checkpoint inventory

| Durable authority | Concrete store/schema | Owning ADR(s) |
|---|---|---|
| Canonical execution state/history | Reth datadir, canonical headers/blocks/receipts and generated EVM storage slots | ADR-B-NOD-001, ADR-B-GEN-001, ADR-B-EVM-003 and ADR-B-EVM-004 plus each module owner |
| Consensus engine state | configured consensus storage/freezer/journal partitions | ADR-B-CNS-001 and ADR-B-OPS-001 |
| Finalized parent proof archive | MDBX `OutbeCertifiedParentFinalizationRecords` and `OutbeCertifiedParentNotarizationRecords` | ADR-B-CNS-001 and ADR-B-CNS-003 |
| Validator/DKG secret material | signing/EVM key references, `dkg_share.hex`, `dkg_polynomial.hex`, `dkg_output.hex` and enclave-sealed state | ADR-S-KEY-001, ADR-B-CNS-002, ADR-S-TEE-001 and ADR-B-OPS-001 |
| Authenticated compressed-entity tree | `${datadir}/compressed_entities/smt`, schema v3, environment identity, catalog/shard trees and `last_applied` marker | ADR-B-OCD-008 through ADR-B-OCD-015 |
| Finalized Mongo projection state | logical database collections `projection_state`, `projection_writer_lease`, `tributes`, `tributes_by_owner`, `tributes_by_day`, `nods`, `nod_buckets`, `nods_by_owner` | ADR-B-OCD-002 through ADR-B-OCD-005, ADR-B-OCD-014 and ADR-B-OPS-001 |
| Projection checkpoint | `projection_state/offchain_data`, atomically committed with domain/index mutations | ADR-B-OCD-004 and ADR-B-OCD-014 |
| Contract deployment state | signed deployment manifest plus on-chain code/proxy/role/wiring state | ADR-B-DEP-001 |

In-memory caches, candidate overlays, telemetry snapshots and PID files are not durable
authorities. They must be reconstructible from a row above or explicitly treated as
availability-only state. Every new table, collection, key file or checkpoint must add a
row or extend an existing row in the same change that introduces it.

## Open questions and technical debt

- Generate this ledger mechanically from Cargo metadata, Solidity AST/artifacts, binary
  targets, RPC registration and precompile/lifecycle registries; current mapping is
  reviewer-maintained Markdown.
- Verify the 58-row Cargo table against metadata in CI and fail on unmapped additions.
- Generate ABI-selector/layout manifests for every stateful precompile and compare them
  with checked-in Solidity interfaces. Dispatch, lifecycle, RPC and MCP tool registries
  are now explicit above but remain reviewer-maintained.
- Extend the persistent-store inventory to generated per-field EVM slot families and
  imported Reth/Commonware tables using machine-generated schema manifests.
- Classify scripts/examples as production, operator, verification or illustrative and
  prevent illustrative code from being cited as production evidence.
- Reconcile dependency licenses/advisories and external pinned revisions into the
  ADR-B-RLS-001 release evidence manifest.
