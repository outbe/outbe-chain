# End-to-end evidence inventory

This inventory prevents executable flows from becoming invisible merely because
they do not use the Rust/Cucumber harness. It classifies evidence by execution
boundary; a lower level is useful evidence, but must not be described as a live
protocol-flow result.

## Live multi-node scenarios

| Runner | Boundary | PFS evidence | Canonical command |
|---|---|---|---|
| `crates/testing/e2e-harness/features/tribute_projection.feature` | Four validators, mock TEE, isolated MongoDB projections | PFS-001-01, -02, -03, -05 | `mise run e2e` |
| `crates/testing/e2e-harness/features/update_operator.feature` | Four validators and real vote/update lifecycle | PFS-005-01, -09 | `mise run e2e` |
| lifecycle, DKG, downtime, restart and stale-join harness features | Mutable four-validator committee and mock TEE | PFS-006-01, -02, -03, -04, -06, -09; see each row's partial-coverage note | `mise run e2e` |
| `crates/testing/e2e-harness/features/follower_upstream.feature` | Followers, validator recovery and warm promotion | PFS-008-01 through -04 | `mise run e2e` |
| `crates/testing/e2e-harness/features/zerofee.feature` | Fresh four-validator localnet, native Alloy EIP-7702 signing | PFS-007-01 through -06 | `mise run e2e` |

The nightly workflow runs the canonical harness. PFS rows tagged
documentation-only are requirements, not claims of executable coverage.

## In-process cross-module scenarios

| Test owner | Evidence supplied | PFS relationship |
|---|---|---|
| `crates/core/e2e/tests/wwd_lysis_nod_gratis.rs` | WWD to Lysis, Nod and Gratis state transitions | Partial PFS-002 |
| `crates/core/e2e/tests/governance_lifecycle.rs` | Vote lifecycle and duplicate-ballot invariants | Partial PFS-005 |
| `crates/core/e2e/tests/update_flow_spec.rs` | Update scheduling, activation and ordering/error edges | Partial PFS-005 |
| `crates/core/credisfactory/src/tests/e2e.rs` | Pledge, Credis repayments and reclaim plus invalid-input edges | Partial PFS-003 |
| `crates/blockchain/evm/tests/e2e_system_tx.rs` | System-transaction ordering, wire layout and gas behavior | ADR-level blockchain evidence; not a complete PFS |
| `bin/outbe-tee-enclave/tests/dkg_e2e.rs` | Four enclave peers over real UDS and Noise-IK transport | Partial PFS-006 and TEE/DKG ADR evidence; not live nodes |

Run these with their owning Cargo packages. They compose production modules in
one process, so they cannot prove networking, finality, restart, projection or
multi-node convergence unless a matrix row explicitly says otherwise.

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
