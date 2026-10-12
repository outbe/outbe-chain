# Test contracts

Shared Solidity fixtures for the E2E harness. `MockHyperlaneIsm` and
`MockHyperlaneIcaRouter` give the HyperlaneController precompile an owner
surface to drive.

Run from this directory:

```sh
forge soldeer install
forge build
```

The harness deploys both from `src/MockHyperlaneController.sol`.
