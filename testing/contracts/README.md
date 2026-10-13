# Test contracts

Shared Solidity fixtures for the E2E harness. `MockHyperlaneIsm` and
`MockHyperlaneIcaRouter` give the HyperlaneController precompile an owner
surface to drive.

Build from this directory with `forge build`.

The harness deploys both from `src/MockHyperlaneController.sol`.
