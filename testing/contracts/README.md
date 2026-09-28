# Test contracts

Shared Solidity fixtures for the E2E harness. `MockSmartAccount` gives its user
and CCA equal access to arbitrary calls and native transfers.

Run from this directory:

```sh
forge soldeer install
forge test
```

The harness deploys `src/MockSmartAccount.sol:MockSmartAccount` from this project.
