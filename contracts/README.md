# Outbe Contracts 

Solidity smart contracts for the Outbe 

## Requirements

- **[Foundry](https://getfoundry.sh)** (`forge` / `cast` / `anvil`) 
- **Node.js ≥ 20 + Yarn 4**  for `intex` (Hardhat toolchain);

## Existing projects

| Project | Description | Solc | Toolchain |
| --- | --- | --- | --- |
| [intent](./intent) | ERC-7683 cross-chain intent settlement with an auction-based solver selection mechanism (LayerZero). | 0.8.30 | Forge + soldeer |
| [tokens](./tokens) | Outbe stablecoin bridge flow built on ERC-7786 adapters and ERC-7802 tokens. | 0.8.30 | Forge + soldeer |
| [vault](./vault) | Non-custodial ERC-4626 vault provider, based on Morpho Vaults V2. | 0.8.30 | Forge + soldeer |
| [smart-account](./smart-account) | ERC-4337 smart-account solution built on the ZeroDev Kernel. | 0.8.30 | Forge + soldeer |
| [precompiles](./precompiles) | Solidity interfaces for the outbe-chain stateful precompiles (e.g. Oracle). | 0.8.30 | Forge |
| [intex](./intex) | Intex NFT (ERC-1155) / auction contracts over the ERC-7786 crosschain hub. | 0.8.30 | Forge + Hardhat (only tasks) |

Each sub-project documents its own design and usage in its `README.md`.

## Project structure

Every sub-project follows the same layout:

```
contracts/<name>/
  src/            # contracts
  test/           # forge tests
  script/         # forge deploy/ops scripts
  abi-export/     # exported ABIs (mise run export-abi)
  foundry.toml
  mise.toml
  README.md
```



## Tooling

Tasks are run with [mise](https://mise.jdx.dev/). The same tasks run in any sub-project
directory, or across **all** sub-projects from `contracts/`:

```sh
mise run install      # forge soldeer install (restore deps)
mise run build        # forge build
mise run test         # forge test
mise run fmt          # forge fmt
mise run lint         # forge lint
mise run export-abi   # export ABIs to abi-export/
```

Run `mise tasks` to list every task in the current directory. Per-project extras
(deploy/configure tasks) live in each sub-project's `mise.toml`.

Dependencies are managed with **[soldeer](https://soldeer.xyz)**  `intex` uses `yarn` (Hardhat).


## Add a new project

1. Create `contracts/<name>/` with the standard layout above.
2. Copy a `foundry.toml` from an existing Forge sub-project (keep the shared compiler profile;
   `optimizer_runs` is per-project) and a `mise.toml`.
3. Add dependencies with `forge soldeer install <dep>~<version>`  and wire remappings in `foundry.toml`.
