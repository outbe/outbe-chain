# Architecture Decision Records

This is the canonical project-wide architecture index. ADR governance and the
architectural evidence contract are defined in [README.md](README.md). End-to-end behavior
crossing module authorities belongs to [Protocol Flow Specifications](../flows/index.md).
The package/entrypoint-to-owner inventory is maintained in the
[architecture coverage ledger](coverage.md).

## Identity and ordering

The canonical ADR code is `ADR-D-MMM-NNN`:

- `D` is the architecture-space code: `B`, `S`, or `C`;
- `MMM` is a registered, stable three-letter architecture-owner code; and
- `NNN` is a zero-padded, three-digit sequence local to the `(D, MMM)` pair.

In expanded form, every ADR therefore has identity
`ADR-<space>-<module>-<sequence>`:

| Prefix | Architecture Space | Owns |
|---|---|---|
| `B` | [Blockchain](blockchain/) | Node, consensus/finality, execution/EVM, txpool, RPC and authenticated persistence/projection substrate |
| `S` | [System](system/) | Network operation/evolution mechanisms: scheduling, validators, economics, Oracle, TEE, governance/update, fees and shared cryptography |
| `C` | [Core](core/) | Consume-to-Gain protocol and business state |

Space is determined by architectural responsibility, not current crate path. Thus
Governance is System even while its crate is physically under `crates/core`.

The physical filename is
`docs/adr/<space>/ADR-<space-prefix>-<module>-<sequence>-<slug>.md`.
The complete ADR identity is therefore visible without its parent directory. For
example, `ADR-B-OCD-014` lives at
`docs/adr/blockchain/ADR-B-OCD-014-cross-store-crash-restart-reconciliation.md`.

`module` is a stable three-letter architectural owner, not necessarily a crate name.
Sequences follow dependency and protocol evolution order and are dense independently
inside each `(space, module)` pair. A module with `N` entries uses exactly `001..N`,
including `Planned` entries; gaps are forbidden. During this reconstruction a module
may be atomically renumbered. Once accepted, its evolution appends `N+1` and existing
identifiers stay stable.

## Module code registry

Codes are explicit architecture namespaces. They are never inferred from directory
or crate names, and cross-module runtime sagas remain PFS documents.

| Space | Code | Architectural owner |
|---|---|---|
| B | `NOD` | Node process construction and ownership |
| B | `WIR` | Consensus-visible identifiers, headers and wire formats |
| B | `CRY` | Shared cryptographic profiles and canonical codecs |
| B | `GEN` | Genesis, chain identity and schema activation |
| B | `CNS` | Consensus, DKG and finalized execution delivery |
| B | `EVM` | Block execution, EVM extensions, storage and dispatch |
| B | `TXP` | Transaction-pool admission and ordering |
| B | `RPC` | RPC consistency and verifiable reads |
| B | `CLI` | Operator CLI and transaction intent |
| B | `MCP` | Local agent tools and signed transaction intent |
| B | `DEP` | Deterministic contract deployment, wiring and upgrades |
| B | `OPS` | Node deployment, configuration and data operations |
| B | `RLS` | Reproducible build, supply-chain and release provenance |
| B | `OCD` | Off-chain data, compressed entities, projection and proofs |
| B | `CAP` | Resource metering and validator capacity |
| B | `SUP` | Supervision, readiness and observability |
| B | `TST` | Production verification and evidence |
| B | `XCH` | Generic cross-chain message transport |
| B | `SMA` | Modular smart-account execution and custody |
| S | `CYC` | Protocol cycle scheduling |
| S | `VAL` | Validator registry and eligibility |
| S | `STK` | Bonded stake and unbonding |
| S | `RWD` | Validator reward settlement |
| S | `SLS` | Offense evidence and slashing |
| S | `KEY` | Validator key generation and custody |
| S | `ACC` | Certified accounting progress |
| S | `EMI` | Emission limit and allocation |
| S | `ORC` | Oracle state and feeder delivery |
| S | `TEE` | Enclave protocol, identity and key epochs |
| S | `GOV` | Governance, voting and protocol activation |
| S | `FEE` | Fee waiver and sponsorship policy |
| S | `ZKP` | ZK verification and proof hash profile |
| C | `TRB` | Tribute ledger and offer admission |
| C | `NOD` | Nod ledger, issuance and qualification |
| C | `GRT` | Gratis ledger, workflows and shielded pool |
| C | `MET` | Metadosis WorldwideDay state machine |
| C | `AGR` | AgentReward accounting |
| C | `LYS` | Lysis transformation |
| C | `FID` | Fidelity cohorts |
| C | `PRM` | Promis ledger, factory and allocation limit |
| C | `CRD` | Credis ledger and factory |
| C | `VLT` | VaultProvider liquidity authority |
| C | `TOK` | Native, synthetic and bridged settlement tokens |
| C | `INX` | Intex ledger and factory |
| C | `GEM` | Gem ledger and factory |
| C | `DES` | Desis auction state machine |
| C | `INT` | Cross-chain intent, solver auction and collateral |
| C | `LBM` | Liquidity-book math and bin index |
| C | `POW` | Shared entity-mining proof of work |

Core evolution is explicitly ordered:

```text
Tribute -> Nod -> Gratis -> Metadosis/AgentReward -> Lysis
        -> Fidelity -> Promis -> Credis/Vault -> Intex -> Gem -> Desis
```

This is architectural dependency/evolution, not a PFS sequence. Actual cross-module
runtime sagas remain in `docs/flows`.

## Status vocabulary

| Status | Meaning |
|---|---|
| Proposed | Decision reconstructed or proposed but not accepted as deployed truth. |
| Accepted | Normative decision; implementation may still have explicit debt. |
| Implemented | Accepted and supported by inspected production-interface evidence. |
| Superseded | Replaced by named ADRs/PFS and no longer normative. |
| Rejected | Deliberately not adopted. |
| Planned | Required coverage boundary whose document is not written yet. |

`Implemented` is not an architecture-conformance verdict. Each ADR must still expose bypasses, illegal
states, partial effects and missing production evidence under the exact heading
`## Open questions and technical debt`.

## Blockchain Space

| ADR | Decision | Principal scope | Status |
|---|---|---|---|
| [ADR-B-NOD-001](blockchain/ADR-B-NOD-001-single-process-node-lifecycle.md) | Single-process node lifecycle and ownership | `bin/outbe-chain`, node, engine | Proposed |
| [ADR-B-WIR-001](blockchain/ADR-B-WIR-001-protocol-identifiers-and-consensus-wire-contract.md) | Protocol identifiers, header/wire and schedule registry | primitives/wire | Proposed |
| [ADR-B-CRY-001](blockchain/ADR-B-CRY-001-cryptographic-profiles-namespaces-and-canonical-codecs.md) | Cryptographic profiles, namespaces and canonical codecs | consensus/SMT/ABI/crypto dependencies | Proposed |
| [ADR-B-GEN-001](blockchain/ADR-B-GEN-001-genesis-chain-identity-and-schema-activation.md) | Reproducible genesis, chain identity and schema activation | chain spec/genesis/migrations | Proposed |
| [ADR-B-CNS-001](blockchain/ADR-B-CNS-001-simplex-consensus-and-finality.md) | Simplex consensus and finality | consensus | Proposed |
| [ADR-B-CNS-002](blockchain/ADR-B-CNS-002-dkg-and-committee-activation.md) | DKG and committee activation boundary | consensus DKG | Proposed |
| [ADR-B-CNS-003](blockchain/ADR-B-CNS-003-consensus-execution-delivery.md) | Acknowledged consensus-to-execution delivery | consensus/engine | Proposed |
| [ADR-B-EVM-001](blockchain/ADR-B-EVM-001-block-lifecycle-and-system-transactions.md) | Block lifecycle and system transaction order | EVM executor | Proposed |
| [ADR-B-EVM-002](blockchain/ADR-B-EVM-002-outbe-evm-extension-and-call-frame-contract.md) | Outbe EVM registry/context/call frames | EVM integration | Proposed |
| [ADR-B-EVM-003](blockchain/ADR-B-EVM-003-stateful-precompile-storage-capability.md) | Journaled precompile storage capabilities | storage provider seam | Proposed |
| [ADR-B-EVM-004](blockchain/ADR-B-EVM-004-generated-storage-layout-and-abi-dispatch.md) | Generated storage layout and ABI dispatch | macros | Proposed |
| [ADR-B-EVM-005](blockchain/ADR-B-EVM-005-stateful-runtime-module-contract.md) | Common stateful module contract | core/system runtime seam | Proposed |
| [ADR-B-TXP-001](blockchain/ADR-B-TXP-001-transaction-pool-admission-and-ordering.md) | Txpool admission and proposer ordering | txpool/payload | Proposed |
| [ADR-B-RPC-001](blockchain/ADR-B-RPC-001-rpc-consistency-and-verifiable-reads.md) | RPC consistency and verifiable reads | RPC | Proposed |
| [ADR-B-CLI-001](blockchain/ADR-B-CLI-001-operator-cli-safety-contract.md) | Operator CLI intent/safety contract | `outbe-cli` | Proposed |
| [ADR-B-MCP-001](blockchain/ADR-B-MCP-001-local-agent-tool-transaction-safety.md) | Local agent tool transaction safety | `mcp` package | Proposed |
| [ADR-B-DEP-001](blockchain/ADR-B-DEP-001-deterministic-contract-deployment-wiring-and-upgrades.md) | Deterministic deployment/wiring/upgrades | Solidity deploy surfaces | Proposed |
| [ADR-B-OPS-001](blockchain/ADR-B-OPS-001-node-deployment-configuration-and-data-operations.md) | Node deployment, configuration and data operations | systemd/localnet/Mongo/monitoring | Proposed |
| [ADR-B-RLS-001](blockchain/ADR-B-RLS-001-reproducible-build-supply-chain-and-release-provenance.md) | Reproducible build, supply-chain and release provenance | locks/CI/packages/images/SBOM | Proposed |
| [ADR-B-OCD-001](blockchain/ADR-B-OCD-001-offchain-storage-facade.md) | Off-chain storage capability facade | offchain-storage | Proposed |
| [ADR-B-OCD-002](blockchain/ADR-B-OCD-002-tribute-nod-body-boundary.md) | Tribute/Nod body and repository boundary | compressed entities/domain repositories | Proposed |
| [ADR-B-OCD-003](blockchain/ADR-B-OCD-003-full-body-receipt-events.md) | Complete projection records in receipts | EVM events/ABI | Proposed |
| [ADR-B-OCD-004](blockchain/ADR-B-OCD-004-reth-exex-mongo-projection.md) | Finalized Reth ExEx Mongo projection | offchain-data | Proposed |
| [ADR-B-OCD-005](blockchain/ADR-B-OCD-005-mongo-execution-reads.md) | Verified Mongo execution reads | runtime/RPC/Mongo | Proposed |
| [ADR-B-OCD-006](blockchain/ADR-B-OCD-006-body-commitment-and-verification.md) | Canonical CE identity, body and commitment | compressed entities | Proposed |
| [ADR-B-OCD-007](blockchain/ADR-B-OCD-007-generic-lifecycle-and-journaled-overlay.md) | CE lifecycle and journaled overlay | compressed entities runtime | Proposed |
| [ADR-B-OCD-008](blockchain/ADR-B-OCD-008-basic-unsharded-smt.md) | Vendored authenticated SMT and finalized persistence | CE MDBX/tree | Proposed |
| [ADR-B-OCD-009](blockchain/ADR-B-OCD-009-smt-sharding.md) | Deterministic SMT sharding | CE tree | Proposed |
| [ADR-B-OCD-010](blockchain/ADR-B-OCD-010-collections-and-root-catalog.md) | Collections and Root Catalog | CE topology | Proposed |
| [ADR-B-OCD-011](blockchain/ADR-B-OCD-011-partition-retirement.md) | Finalized collection retirement | CE/Lysis storage seam | Proposed |
| [ADR-B-OCD-012](blockchain/ADR-B-OCD-012-header-root-carrier.md) | Execution-sealed CE root in block header | header/execution seam | Proposed |
| [ADR-B-OCD-013](blockchain/ADR-B-OCD-013-proofs-and-verified-point-reads.md) | Authenticated point-read proofs | CE proof/RPC seam | Proposed |
| [ADR-B-OCD-014](blockchain/ADR-B-OCD-014-cross-store-crash-restart-reconciliation.md) | Cross-store crash/restart reconciliation | Reth/consensus/Mongo/CE checkpoints | Proposed |
| [ADR-B-OCD-015](blockchain/ADR-B-OCD-015-authenticated-snapshot-bootstrap-and-state-recovery.md) | Authenticated snapshot bootstrap and state recovery | node/bootstrap/import tooling | Proposed |
| [ADR-B-CAP-001](blockchain/ADR-B-CAP-001-resource-metering-and-capacity-closure.md) | Resource metering and capacity closure | execution/CE/queues/external services | Proposed |
| [ADR-B-SUP-001](blockchain/ADR-B-SUP-001-supervision-failure-taxonomy-readiness-and-observability.md) | Supervision, failure taxonomy, readiness and observability | supervisors/probes/metrics | Proposed |
| [ADR-B-TST-001](blockchain/ADR-B-TST-001-production-verification-and-evidence-architecture.md) | Production verification and evidence architecture | test layers/CI/evidence ledger | Proposed |
| [ADR-B-XCH-001](blockchain/ADR-B-XCH-001-erc7786-cross-chain-message-transport.md) | ERC-7786 authenticated message transport | cross-chain adapters | Proposed |
| [ADR-B-SMA-001](blockchain/ADR-B-SMA-001-modular-smart-account-authorization-and-bundle-custody.md) | Smart-account authorization and bundle custody | ERC-4337/ERC-7579/Kernel | Proposed |

## System Space

| ADR | Decision | Principal scope | Status |
|---|---|---|---|
| [ADR-S-CYC-001](system/ADR-S-CYC-001-deterministic-cycle-scheduling.md) | Deterministic Cycle scheduling | cycle | Proposed |
| [ADR-S-VAL-001](system/ADR-S-VAL-001-validator-registry-and-committee-eligibility.md) | Validator identity/lifecycle/eligibility | validatorset | Proposed |
| [ADR-S-STK-001](system/ADR-S-STK-001-bonded-stake-and-unbonding-ledger.md) | Bonded stake and unbonding | staking | Proposed |
| [ADR-S-RWD-001](system/ADR-S-RWD-001-finalized-participation-and-reward-settlement.md) | Participation and validator rewards | rewards | Proposed |
| [ADR-S-SLS-001](system/ADR-S-SLS-001-offense-evidence-and-slashing.md) | Offense evidence and slashing | slashindicator | Proposed |
| [ADR-S-KEY-001](system/ADR-S-KEY-001-validator-key-generation-and-secret-custody.md) | Validator key generation/custody | keygen/consensus keys | Proposed |
| [ADR-S-ACC-001](system/ADR-S-ACC-001-accounting-progress.md) | Certified-parent accounting progress | accounting | Proposed |
| [ADR-S-EMI-001](system/ADR-S-EMI-001-emission-limit-formula.md) | Daily emission limit/allocation | emissionlimit | Proposed |
| [ADR-S-ORC-001](system/ADR-S-ORC-001-oracle-state-and-tally.md) | Oracle registry, tally and histories | oracle | Proposed |
| [ADR-S-ORC-002](system/ADR-S-ORC-002-oracle-feeder-ingestion-and-delivery.md) | External feed ingestion/delivery | outbe-feeder | Proposed |
| [ADR-S-TEE-001](system/ADR-S-TEE-001-node-enclave-protocol-and-execution-boundary.md) | Node/enclave execution boundary | tee/enclave | Proposed |
| [ADR-S-TEE-002](system/ADR-S-TEE-002-enclave-identity-and-offer-key-registry.md) | Enclave identity and offer-key epochs | teeregistry | Proposed |
| [ADR-S-GOV-001](system/ADR-S-GOV-001-governance-editorial-registry.md) | Governance editorial registry | governance | Proposed |
| [ADR-S-GOV-002](system/ADR-S-GOV-002-executable-vote-state-machine.md) | Executable vote FSM | vote | Proposed |
| [ADR-S-GOV-003](system/ADR-S-GOV-003-scheduled-protocol-update-activation.md) | Scheduled protocol activation | update | Proposed |
| [ADR-S-FEE-001](system/ADR-S-FEE-001-zero-fee-policy.md) | Zero-fee/sponsorship policy | zerofee | Proposed |
| [ADR-S-ZKP-001](system/ADR-S-ZKP-001-versioned-zk-verifier-registry-and-crs-trust.md) | Circuit/VK registry and CRS trust | zkproof verifier | Proposed |
| [ADR-S-ZKP-002](system/ADR-S-ZKP-002-poseidon-bn254-hash-contract.md) | Poseidon BN254 hash profile | zkproof Poseidon | Proposed |

## Core Space

| ADR | Decision | Principal scope | Status |
|---|---|---|---|
| [ADR-C-TRB-001](core/ADR-C-TRB-001-authenticated-tribute-ledger.md) | Authenticated Tribute ledger | tribute | Proposed |
| [ADR-C-TRB-002](core/ADR-C-TRB-002-encrypted-tribute-offer-admission.md) | Encrypted Tribute offer admission | tributefactory | Proposed |
| [ADR-C-NOD-001](core/ADR-C-NOD-001-authenticated-nod-ledger-and-qualification.md) | Authenticated Nod ledger/qualification | nod | Proposed |
| [ADR-C-NOD-002](core/ADR-C-NOD-002-nod-issuance-and-gratis-mining-orchestration.md) | Nod issuance and Gratis mining | nodfactory | Proposed |
| [ADR-C-GRT-001](core/ADR-C-GRT-001-gratis-ledger.md) | Gratis earned-value ledger | gratis | Proposed |
| [ADR-C-GRT-002](core/ADR-C-GRT-002-gratisfactory-workflows.md) | Gratis business workflows | gratisfactory | Proposed |
| [ADR-C-GRT-003](core/ADR-C-GRT-003-gratispool-shielded-notes.md) | Shielded Gratis notes | gratispool | Proposed |
| [ADR-C-MET-001](core/ADR-C-MET-001-metadosis-worldwide-day-fsm.md) | Metadosis WorldwideDay FSM | metadosis | Proposed |
| [ADR-C-AGR-001](core/ADR-C-AGR-001-agent-reward-accounting.md) | AgentReward accounting | agentreward | Proposed |
| [ADR-C-LYS-001](core/ADR-C-LYS-001-lysis-tribute-to-nod-transformation.md) | Tribute-to-Nod Lysis | lysis | Proposed |
| [ADR-C-FID-001](core/ADR-C-FID-001-fidelity-cohort-ledger.md) | Fidelity cohorts/retention | fidelity | Proposed |
| [ADR-C-PRM-001](core/ADR-C-PRM-001-promis-ledger.md) | Promis ledger | promis | Proposed |
| [ADR-C-PRM-002](core/ADR-C-PRM-002-promis-factory-conversions.md) | Promis conversions | promisfactory | Proposed |
| [ADR-C-PRM-003](core/ADR-C-PRM-003-promis-unallocated-limit.md) | Unallocated Promis limit | promislimit | Proposed |
| [ADR-C-CRD-001](core/ADR-C-CRD-001-credis-position-ledger.md) | Credis position/installment FSM | credis | Proposed |
| [ADR-C-CRD-002](core/ADR-C-CRD-002-credis-factory-orchestration.md) | Credis orchestration | credisfactory | Proposed |
| [ADR-C-VLT-001](core/ADR-C-VLT-001-vault-provider-liquidity-authority.md) | Vault liquidity authority | vaultprovider | Proposed |
| [ADR-C-TOK-001](core/ADR-C-TOK-001-native-wrapped-and-synthetic-token-contracts.md) | Native/wrapped/synthetic token issuance | Solidity token contracts | Proposed |
| [ADR-C-TOK-002](core/ADR-C-TOK-002-fungible-token-cross-chain-custody.md) | Fungible cross-chain token custody | ERC-7786 token bridge | Proposed |
| [ADR-C-INX-001](core/ADR-C-INX-001-intex-series-ledger.md) | Intex series ledger | intex | Proposed |
| [ADR-C-INX-002](core/ADR-C-INX-002-intex-factory-orchestration.md) | Intex orchestration | intexfactory | Proposed |
| [ADR-C-INX-003](core/ADR-C-INX-003-cross-chain-intex-erc1155-ledger.md) | Cross-chain Intex ERC-1155 ledger | Solidity Intex ledger | Proposed |
| [ADR-C-INX-004](core/ADR-C-INX-004-intex-erc1155-cross-chain-bridge.md) | Intex ERC-1155 bridge/recovery | Solidity NFT bridge | Proposed |
| [ADR-C-INX-005](core/ADR-C-INX-005-target-intex-commit-reveal-auction.md) | Target commit/reveal auction | Solidity IntexAuction | Proposed |
| [ADR-C-INX-006](core/ADR-C-INX-006-target-intex-bid-escrow-and-proceeds.md) | Target bid escrow and proceeds | Solidity EscrowAdapter | Proposed |
| [ADR-C-INX-007](core/ADR-C-INX-007-intex-cross-chain-routing-and-settlement-outboxes.md) | Intex cross-chain inbox/outbox routing | Origin/Target routers | Proposed |
| [ADR-C-GEM-001](core/ADR-C-GEM-001-gem-ledger.md) | Gem ledger | gem | Proposed |
| [ADR-C-GEM-002](core/ADR-C-GEM-002-gem-factory-orchestration.md) | Gem orchestration | gemfactory | Proposed |
| [ADR-C-DES-001](core/ADR-C-DES-001-desis-cross-chain-auction-state-machine.md) | Desis auction FSM | desis | Proposed |
| [ADR-C-INT-001](core/ADR-C-INT-001-intent-order-settlement-and-replay.md) | Intent origin/destination settlement | ERC-7683 Router | Proposed |
| [ADR-C-INT-002](core/ADR-C-INT-002-intent-solver-commit-reveal-auction.md) | Intent solver auction | Solidity Auction | Proposed |
| [ADR-C-INT-003](core/ADR-C-INT-003-solver-collateral-custody-and-slashing.md) | Solver collateral custody/slashing | SolverEscrow/allocators | Proposed |
| [ADR-C-LBM-001](core/ADR-C-LBM-001-liquidity-book-fixed-point-math-and-bin-index.md) | LB math and occupied-bin index | shared core math | Proposed |
| [ADR-C-POW-001](core/ADR-C-POW-001-shared-entity-mining-proof-of-work.md) | Entity-mining PoW | shared core admission | Proposed |

## Legacy reconciliation

The older `/adr` series is frozen historical input and is not a fourth Architecture
Space. Its detailed ADR-001–013 decisions were migrated one-for-one into
ADR-B-OCD-001 through ADR-B-OCD-013; its planned crash recovery and snapshot closure became
ADR-B-OCD-014 through ADR-B-OCD-015, while capacity closure became ADR-B-CAP-001. Current prefixed ADRs always win; two editable normative copies are
forbidden. The historical directory may be removed after migration review because
Git already preserves its provenance.

Deleted pre-space aggregates previously numbered unprefixed ADR-026, ADR-027 and
ADR-028 are replaced by granular System/Core ADRs and PFS-001, PFS-005 and PFS-006. Those
legacy unprefixed identifiers are unrelated to dense space-local identifiers such as
ADR-B-OCD-011.

## Coverage rules

- One ADR owns one primary authority, state machine or critical external boundary.
- A cross-space saga is a PFS, not an aggregate ADR.
- Every ADR contains exact production evidence and `## Open questions and technical debt`.
- A crate mention is not coverage: public mutations, persistent state, entrypoints,
  failure paths, replay, activation and tests must be inspected.
- Planned rows and unreconciled legacy records prevent a claim of full coverage.

## Open questions and technical debt

- Reconcile every ADR/PFS evidence claim into the ADR-B-TST-001 verification ledger.
- Remove the frozen `/adr` duplicate after reviewing the migrated ADR-B-OCD-001 through ADR-B-OCD-015
  documents, then replace remaining bare legacy references in code/tests/operations.
- Add a link checker that validates every ADR/PFS identifier and relative path.
- Add CI enforcing prefix/path/header agreement and unique identifiers.
- Decide and document reviewer/CODEOWNERS policy per Architecture Space.
- Audit physical crate placement against the conceptual spaces; Governance is the
  first known mismatch and may merit a future code move, but its ADR remains System.
- Update all code comments/test names that still cite legacy unprefixed ADR ids.
