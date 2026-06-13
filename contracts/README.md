# Outbe Contracts 

Solidity smart contracts for the Outbe 

## Requirements

- **[Foundry](https://getfoundry.sh)** (`forge` / `cast` / `anvil`) 
- **Node.js ≥ 20 + Yarn 4**  for `intex` (Hardhat toolchain);

## Existing projects

| Project | Description | Solc | Toolchain |
| --- | --- | --- | --- |
| [intent](./intent) | ERC-7683 cross-chain intent settlement with an auction-based solver selection mechanism (LayerZero). | 0.8.30 | Forge + soldeer |
| [oft](./oft) | Outbe stablecoin bridge flow built on LayerZero OFT v2 (adapters & tokens). | 0.8.30 | Forge + soldeer |
| [vault](./vault) | Non-custodial ERC-4626 vault provider, based on Morpho Vaults V2. | 0.8.30 | Forge + soldeer |
| [smart-account](./smart-account) | ERC-4337 smart-account solution built on the ZeroDev Kernel. | 0.8.30 | Forge + soldeer |
| [precompiles](./precompiles) | Solidity interfaces for the outbe-chain stateful precompiles (e.g. Oracle). | 0.8.30 | Forge |
| [intex](./intex) | Intex NFT (ERC-1155) / auction cross-chain contracts. | 0.8.30 | Hardhat + Forge |

Each sub-project documents its own design and usage in its `README.md`.

## Project structure

Every sub-project follows the same layout:

```
contracts/<name>/
  src/            # contracts (intex uses contracts/)
  test/           # forge tests
  script/         # forge deploy/ops scripts
  abi-export/     # exported ABIs (make export-abi)
  foundry.toml
  Makefile
  README.md
```



## Tooling

The same `make` targets run in any sub-project directory, or across **all** sub-projects
from `contracts/`:

```sh
make install      # forge soldeer install (restore deps)
make build        # forge build
make test         # forge test
make fmt          # forge fmt
make lint         # forge lint
make export-abi   # export ABIs to abi-export/
```

Per-project extras (deploy/configure targets) live in each sub-project's Makefile.

Dependencies are managed with **[soldeer](https://soldeer.xyz)**  `intex` uses `yarn` (Hardhat).


## Add a new project

1. Create `contracts/<name>/` with the standard layout above.
2. Copy a `foundry.toml` from an existing Forge sub-project (keep the shared compiler profile;
   `optimizer_runs` is per-project) and a `Makefile`.
3. Add dependencies with `forge soldeer install <dep>~<version>`  and wire remappings in `foundry.toml`.
