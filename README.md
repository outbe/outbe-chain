# Outbe Chain

Public EVM-compatible blockchain built on [Reth](https://github.com/paradigmxyz/reth) (execution) + [Commonware Simplex](https://github.com/commonwarexyz/monorepo) (consensus) in a single Rust binary.

```
~2s blocks | Instant BFT finality | Built-in VRF | BLS hybrid signing | Full EVM
```

No HTTP Engine API split: consensus and execution run in one process and talk through in-process Reth engine handles (`fork_choice_updated`, `new_payload`, payload builder). Validator lifecycle, staking, rewards, slashing, and business logic are stateful Rust precompiles; upgrades are hard-fork driven, with no proxy-admin governance.

## Architecture

```
outbe-chain (single binary)
├── Reth SDK ─────────────── Execution Layer
│   ├── Native EVM (Solidity, MetaMask, ethers.js)
│   ├── ZeroFee txpool admission + deterministic priority classes
│   ├── Stateful Rust precompiles (system 0xEE.. / business 0x10.., 0x11.., 0x20..)
│   └── Begin/end-block hooks + OutbeBlockArtifacts in header.extra_data
└── Commonware Simplex ───── Consensus Layer
    ├── BLS-only hybrid scheme (multisig + threshold) → voter attribution + VRF
    ├── VRF leader election (round-robin only at genesis view 1)
    └── Automatic DKG / reshare orchestration on validator-set changes
```

## Repository Layout

```
outbe-chain/
├── bin/
│   ├── outbe-chain/        # Node binary (validator / full-node modes)
│   ├── outbe-cli/          # Operator CLI (validator, staking, rewards, monitor)
│   ├── outbe-keygen/       # Offline BLS / EVM key generation
│   ├── outbe-feeder/       # Oracle price feeder
│   └── outbe-tee-enclave/  # TEE enclave binary
├── crates/
│   ├── blockchain/         # consensus, engine, evm, node, primitives, rpc, txpool, macros
│   ├── system/             # validatorset, staking, rewards, slashindicator, oracle, ...
│   └── core/               # core modules: tribute, gratis, nod, credis, metadosis, ...
├── contracts/              # Solidity interfaces for precompiles + external contracts
├── scripts/                # genesis seeding, testnet bootstrap
└── deploy/                 # systemd units, monitoring
```

## Quick Start

Prerequisites: [`mise`](https://mise.jdx.dev) (provisions the Rust toolchain, Foundry, and cargo tools from `mise.toml`). Run `mise install` once, then `mise tasks` to list every task.

```bash
# 4-validator localnet
mise run build-release
mise run localnet-bootstrap     # BLS keys + genesis.json
mise run localnet-start
mise run localnet-status        # all 4 nodes should advance past block 0

# Verify via RPC
curl -s -X POST http://localhost:8545 -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}'

# Tests
mise run test                   # cargo nextest run --workspace + doctests
mise run test-consensus         # consensus crate only
```

## CLI Tools

```bash
outbe-chain node [flags]                      # run validator or full node
outbe-keygen generate --output-dir <dir>      # BLS12-381 MinPk keypair (offline)
outbe-cli validator register|info|list        # validator lifecycle
outbe-cli staking stake|unstake|claim         # staking flow
outbe-cli rewards pending|claim               # validator rewards
```

Full nodes sync and serve RPC without consensus key material; validators additionally pass `--validator --consensus.signing-key <path>`.

## Documentation

- `docker-compose.yml`, `deploy/` — local testnet and deployment
