# Ape Tests

This directory contains Ape tests for the vault contracts. The tests run
against an already-running local Outbe/Reth RPC node.

## Prerequisites

- Python 3.10+
- A local RPC node listening on `http://127.0.0.1:8545`
- The local chain must use chain id `54322345`
- A funded test account for transactions

The Ape network and Solidity compiler settings are defined in the repository
root `ape-config.yaml`. Contract discovery currently starts from
`contracts/vault`.

## Python Setup

Create and activate a virtual environment from the repository root:

```sh
python3.10 -m venv .venv
source .venv/bin/activate
pip install --upgrade pip
pip install eth-ape
```

Install Ape plugins and dependencies:

```sh
ape plugins install .
ape pm install
```

Create `.env` with the account used by the localhost chain. The tests do not
use a default address; both values must match the funded local account.

```sh
TEST_ADDRESS=0x...
TEST_PRIVATE_KEY=0x...
```

Run the tests:

```sh
ape test
# or
ape test tests/test_vault_e2e.py -q -rs
```

## Vault E2E

`tests/test_vault_e2e.py` checks the direct implementation deployment flow:

- deploy `ERC20Mock`
- deploy `ERC4626Mock` for the ERC20 asset
- deploy `VaultProvider` implementation directly and call `initialize`
- register the vault, liquidity source, and liquidity target
- mint test assets to `TEST_ADDRESS`
- approve and deposit liquidity through `VaultProvider`
- withdraw part of the liquidity to `TokenBundleReceiverMock`
- verify provider share accounting and receiver asset balance

## Troubleshooting

- If the test is skipped, check that `TEST_ADDRESS` and `TEST_PRIVATE_KEY` are
  set and that the account has native balance on the local node.
- If Ape cannot connect, make sure the local RPC node is already running on
  `http://127.0.0.1:8545`.
- If compilation fails after adding new contract folders, update
  `ape-config.yaml` includes, excludes, or remappings so Ape only compiles
  contracts that are valid for this test setup.
