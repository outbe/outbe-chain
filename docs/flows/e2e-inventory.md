# End-to-end evidence inventory

This inventory prevents executable flows from becoming invisible merely because
they do not use the Rust/Cucumber harness. It classifies evidence by execution
boundary; a lower level is useful evidence, but must not be described as a live
protocol-flow result.

## Live multi-node scenarios

| Runner | Boundary | PFS evidence | Canonical command |
|---|---|---|---|
| `testing/e2e-harness/features/tribute_projection.feature` | Four validators, mock TEE, isolated MongoDB projections | PFS-001-01, -02, -03, -05 | `mise run e2e` |
| `testing/e2e-harness/features/l2_zk_gate.feature` | Four validators, mock TEE; harness-held BLS MinPk network key registered in the L2Registry | PFS-001-10, -11 | `mise run e2e` |
| `testing/e2e-harness/features/update_operator.feature` | Four validators, restart boundaries, rejection paths and a real operator binary replacement over preserved datadirs | PFS-005-01, -09 plus named recovery/rejection scenarios | `mise run e2e` |
| lifecycle, DKG, downtime, restart and stale-join harness features | Mutable four-validator committee and TEE, including join/exit/claim accounting, slash idempotency and node/enclave checkpoint recovery | PFS-006-01, -02, -03, -04, -06, -09 | `mise run e2e` |
| `testing/e2e-harness/features/follower_upstream.feature` | Followers, upstream loss/switch, validator recovery and restart-safe warm promotion | PFS-008-01 through -08 | `mise run e2e` |
| `testing/e2e-harness/features/zerofee.feature` | Fresh four-validator localnet, native Alloy EIP-7702 signing, replay/restart/error/day-boundary coverage | PFS-007-01 through -12 | `mise run e2e` |
| `testing/e2e-harness/features/stablecoin_factory_v1.feature` | Fresh four-validator localnet; policy creation, bonded Factory approval, native ledger operations, duplicate-ticker rejection and full-committee same-binary restart | PFS-010-01 through -04 | `mise run e2e` |

The nightly workflow runs the canonical harness. PFS rows tagged
documentation-only are requirements, not claims of executable coverage.

## In-process cross-module scenarios

| Test owner | Evidence supplied | PFS relationship |
|---|---|---|
| `crates/system/cycle/src/tests.rs` Metadosis cases | Real Cycle trigger/lease/command boundary for block-1 profile guard, missed OFFERING, cap forfeiture, day-limit replay consistency and typed Desis rejection | Metadosis H-1..H-3/M-1..M-2 production-interface evidence; process E2E still required |
| `crates/blockchain/evm/tests/ocomp_request_lifecycle.rs` | Canonical proposer/import/historical-replay blocks cover request, certified finality, open, expiry, retry and a quorum-forming block containing validator-authenticated OCOMP system vote carriers | PFS-002 and Metadosis production-adapter evidence; also proves canonical `gas_limit = 30_000` carriers use bounded system work without consuming the user gas lane |
| `testing/e2e/tests/governance_lifecycle.rs` | Vote lifecycle and duplicate-ballot invariants | Partial PFS-005 |
| `testing/e2e/tests/update_flow_spec.rs` | Update scheduling, activation and ordering/error edges | Partial PFS-005 |
| `crates/core/credisfactory/src/tests/e2e.rs` | Pledge, request, settlement, void and the §7 owner rules | Partial PFS-003 |
| `crates/core/credisfactory/src/tests/called.rs` | Daily price-path scan: floor latch, 21-day call streak, void of a lapsed window | Partial PFS-003 |
| `crates/core/cca/src/tests.rs` | CCA registration, performance multiplier and origination units | Partial PFS-003 |
| `crates/core/tributefactory/src/tests.rs` (`l2_zk_gate`) and `crates/system/l2registry/src/tests.rs` | L2Registry registration/toggle/removal invariants and the offer-time BLS zk signature gate (all check outcomes) | Partial PFS-001-10/-11 |
| `crates/blockchain/evm/tests/e2e_system_tx.rs` | System-transaction ordering, wire layout and gas behavior | ADR-level blockchain evidence; not a complete PFS |
| `crates/blockchain/evm/src/handlers.rs` (`pfs_010_05`, `pfs_010_06`, `pfs_010_08`) | Real Vote-to-Factory expiry, proposal execution-error retention and fatal outer-checkpoint rollback | PFS-010-05, -06, -08 |
| `crates/core/stablecoin/src/tests.rs` (`pfs_010_07_*`) | Policy, role, permit-signature and supply-cap failures restore ledger state and events | PFS-010-07 |
| `crates/core/stablecoinfactory/src/tests.rs` (`pfs_010_08_*`) | Failure after every initializer mutation restores marker, token storage, registry and creation event | PFS-010-08 |
| `bin/outbe-tee-enclave/tests/dkg_e2e.rs` | Four enclave peers over real UDS and Noise-IK transport | Partial PFS-006 and TEE/DKG ADR evidence; not live nodes |

Run these with their owning Cargo packages. They compose production modules in
one process, so they cannot prove networking, finality, restart, projection or
multi-node convergence unless a matrix row explicitly says otherwise.

## Pending OCOMP/Metadosis process evidence

The implementation and focused production-adapter tests exist, but no
exact-revision Linux evidence bundle from the full four-validator
request→finality→export→independent execution→full-result votes→q-forming apply
run is checked in by this change. External closure must use four separate
validator domains and the indexed OCOMP/Metadosis packs; direct Lysis fixtures
cannot be relabeled as that evidence.

## Foundry contract suites

Foundry tests are contract-level evidence and are grouped by product boundary:

- `contracts/crosschain/test/*.t.sol`: ERC-7786 and gateway adapters.
- `contracts/intent/test/*.t.sol`: origin/destination settlement, validation,
  routing, allocation and escrow; `RouterE2E.t.sol` is the widest intent slice.
- `contracts/intex/test/foundry/*.t.sol`: auctions, escrow, NFT supply,
  upgrades and invariants.
- `contracts/intex/test/foundry/cross-chain/*.t.sol`: bridge codecs, supply
  conservation, replay protection, routers and failure isolation.
- `contracts/intex/test/foundry/deploy/*.t.sol` and `upgrade/*.t.sol`: deployment
  and upgrade drills.
- `contracts/smart-account/test/*.t.sol`: CCA flow, account approach and
  withdrawal policy.
- `contracts/tokens/test/**/*.t.sol`: native, synthetic and bridged token flows.

These suites supply fragments for PFS-004 and related ADRs. They do not by
themselves prove the Rust runtime plus a live committee plus deployed-contract
flow. Use the contract repository's normal `forge test` commands to execute
them.

## Maintenance rule

When adding or moving an E2E-like test:

1. Assign its strongest honest boundary: live multi-node, in-process module
   composition, contract VM, or documentation-only.
2. Link every asserted PFS row and mark partial assertions in that row.
3. Give every live runner one discoverable `mise` command and a CI owner.
4. Update this inventory and the relevant harness README in the same change.
