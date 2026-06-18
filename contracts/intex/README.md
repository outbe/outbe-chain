# Intex Contracts

[![CI](https://github.com/outbe/outbe-chain/actions/workflows/ci-intex.yml/badge.svg)](https://github.com/outbe/outbe-chain/actions/workflows/ci-intex.yml)

## Overview

Intex is a cross-chain Solidity protocol that runs a **commit–reveal auction across the Outbe chain and BNB** over **LayerZero V2**. The Outbe chain drives the auction; BNB collects sealed bids and locks escrow. After clearing, the result, the Intex-NFT issuance, and refunds are relayed back to BNB.

The Outbe-side logic is split across two contracts:

- **Desis** (demand side) — owns the auction lifecycle and clearing. It sends stage signals to BNB and runs clearing, producing `AUCTION_RESULT` and `REFUND` messages.
- **IntexFactory** (supply side) — issues Intex-NFT series (`issue`) and drives the qualified/called status lifecycle.

BNB-side, `IntexAuction` runs the commit/reveal stages and `EscrowAdapter` locks bidder funds; `TargetMessenger`/`OriginMessenger` carry the cross-chain messages. The same `IntexNFT1155` and ONFT adapters are deployed on both chains so series can be bridged.

## Basic Commands

```bash
yarn install
yarn compile
yarn test
yarn hardhat                      # list all tasks
yarn hardhat <task-name> --help   # options for any task
```

## Runbooks

The `runbook:*` tasks are the canonical way to drive a full cross-chain run end to end. Each run writes a resumable report to `reports/<series-id>/report.{md,json}` with per-step tx hashes, LayerZero delivery proofs, and state assertions — that report is the run artifact.

### Full cycle

```bash
yarn hardhat runbook:auction:all --series-id 20260526 --outbe-network outbeTestnetNew
```

`runbook:auction:all` runs all seven auction phases in order, pausing for **Enter** between each (`--pause false` runs unattended). LayerZero fees are quoted on-chain per send (no `--value` flag). Common options (defaults shown):

| Option | Default | Notes |
|--------|---------|-------|
| `--series-id` | — | `yyyymmdd`; also the report run id. The auction clears at 12:00 UTC of this date |
| `--outbe-network` | `outbeTestnetNew` | Outbe chain |
| `--bnb-network` | `bscTestnet` | BNB chain |
| `--quantity` | `5` | bid quantity |
| `--bid-price` | `60000000` | bid price per Intex (minor units) |
| `--supply` | `100` | issued supply (Intex units) — passed at the `clearing` phase, multiplied by `promisLoadMinor` for Desis |

> **Prefund OriginMessenger before each series.** After the final bid batch lands on Outbe, OriginMessenger calls `Desis.clearAuction` itself in relay mode (msg.value = 0). The three resulting LZ sends (AUCTION_RESULT + ISSUANCE_INSTRUCTIONS + REFUND_INSTRUCTIONS) draw from the messenger's own native float — top it up before kicking off a run (~0.05 native on testnets is plenty per series). `cast send <ORIGIN_MESSENGER> --value 0.05ether` or any plain transfer to its address works (`receive()` accepts native).

### Individual phases

Each phase is its own task so a live run can pause for wall-clock time and LZ delivery. They share one report keyed by `--series-id`, so a run is resumable à la carte. Run in this order:

| # | Task | What it does |
|---|------|--------------|
| 1 | `runbook:auction:start` | Outbe → BNB: create the auction; BNB enters `CommittingBids` |
| 2 | `runbook:auction:commit` | BNB: commit a sealed bid |
| 3 | `runbook:auction:reveal` | Outbe → BNB: open the reveal stage; BNB enters `RevealingBids` |
| 4 | `runbook:auction:reveal-bid` | BNB: approve + reveal the bid, locking escrow |
| 5 | `runbook:auction:clearing` | Outbe → BNB: close reveals; Desis persists supply + issuance |
| 6 | `runbook:auction:relay` | BNB → Outbe: relay the bids. OriginMessenger auto-fires `clearAuction`, sending AUCTION_RESULT + issuance + refund back to BNB |
| 7 | `runbook:auction:verify` | BNB: confirm the series minted on `IntexNFT1155` |

Run `yarn hardhat <task> --help` for the full option list.

### Harness self-test

```bash
yarn hardhat runbook:harness-selftest   # writes a sample run report (smoke test)
```

### Address resolution & keys

The runbook resolves contract addresses in this order: per-contract env overrides `DEMO_ADDR_<CONTRACT>` → `node_modules/@outbe/intex-contracts/dist/addresses/<network>.json` → local `deployed-addresses.json`. External addresses (PaymentToken, VaultProvider, Metadosis, TheCompact) come from `DEMO_ADDR_*` env overrides only. So a run can work against a fresh deploy without editing scripts.

Runner keys are read from `.env`, one per chain: `OUTBE_PRIVATE_KEY` / `OUTBE_RPC_URL` on Outbe, `BSC_TESTNET_PRIVATE_KEY` / `BSC_TESTNET_RPC_URL` on BNB.

## Settlement / Intex Lifecycle

A series moves through `Issued → Qualified → Called → Settled`:

- **Issued** — auction clearing creates the series and mints Issued Intex to bidders. Tokens are tradable/bridgeable; relayer crosschainMint/crosschainBurn and voluntary settle are rejected.
- **Qualified** — `markQualified` flips the series once qualification conditions are met. Holders can bridge to Outbe and voluntarily `settle`.
- **Called** — `markCalled` (cross-chain) sweeps holder balances to Outbe and arms a `callPeriod` deadline within which holders must settle.
- **Settled** — `IntexNFT1155.settle` burns Issued Intex and mints a soulbound `settledTokenId` (1:1). The Outbe Promis precompile later burns Settled Intex (via `IntexNFT1155.burnSettled`) to mint Promis.

`expireSeries` is an action, not a state: it burns remaining Issued tokens and emits `SeriesExpired`; Settled tokens are unaffected.

## Vault Integration

Settlement (Outbe) and post-finalization winner payouts (BNB) route stablecoins through the [outbe-vault](https://github.com/outbe/outbe-vault) `VaultProvider` layer — `VaultProvider.depositLiquidity(asset, amount)` is the single entry point on both chains. The underlying `VaultV2` is gated so only the `VaultProvider` can call it.

- **Outbe-side** `IntexNFT1155.settle` deposits settler stablecoins through the provider.
- **BNB-side** `EscrowAdapter` deposits winner principal (the `paidAmount` split) through the provider inside `finalizeAuction()`; the refund path and the Compact lock / `forcedWithdrawal` reveal flow are unchanged.

Neither contract deposits until the vault operator calls `VaultProvider.addVault(vaultV2)` **and** `addLiquiditySource(<contract>, <slot>)` on each chain — fail-loud reverts (`ReserveVaultNotConfigured` / `InvalidLiquiditySource`) surface a missing step.

Fee-on-transfer stablecoins are unsupported through the provider. The vendored interface is at [`src/vendor/outbe-vault/interfaces/IVaultProvider.sol`](src/vendor/outbe-vault/interfaces/IVaultProvider.sol).

## Deployment

Every implementation contract is a UUPS proxy. The implementation holds only logic and chain-fixed immutables (LayerZero endpoint, endpoint ids, bridged token); all state lives in the proxy under ERC-7201 namespaced storage, and upgrades go through `upgradeToAndCall` without moving the proxy address.

Proxies are deployed through a CREATE3 factory ([`src/factory/Create3Factory.sol`](src/factory/Create3Factory.sol)), so a proxy address depends only on `(factory, deployer, salt)` and not on the implementation init code. Addresses therefore stay fixed across implementation iterations and full network wipes, and are identical across chains — including the LayerZero contracts, whose per-chain endpoint immutable would shift a CREATE2 address but not a CREATE3 one (this is why CREATE3 is used over plain CREATE2). The factory is deployed once per chain through the canonical CREATE2 deployer (`0x4e59…956C`) at a pinned salt, so it lands at the same address everywhere.

Deploy with the Foundry scripts in [`deploy/`](deploy/):

```bash
forge script deploy/DeployBsc.s.sol --rpc-url <bsc-rpc> --broadcast
forge script deploy/DeployOutbe.s.sol --rpc-url <outbe-rpc> --broadcast
```

Env: `DEPLOYER_PRIVATE_KEY`, `LZ_ENDPOINT`, and the remote endpoint id (`OUTBE_EID` for the BNB side, `BNB_EID` for the Outbe side). The deployer is the admin (`DEFAULT_ADMIN_ROLE`) and owner / LZ delegate, so no separate admin/delegate addresses are passed; `RELAYER_ROLE` is granted to the bridge adapters during wiring, not at init. Deploys are idempotent: a contract already present at its predicted address is skipped, so a re-run resumes. Wiring (peers, escrow/compact/vault, roles) is a separate step (see [Other Tasks](#other-tasks)). Bump `SALT_VERSION` in [`deploy/BaseScript.s.sol`](deploy/BaseScript.s.sol) to move every contract to a fresh address set.

### Upgrade safety

- `yarn validate:upgrades` runs the OpenZeppelin upgrades-core storage-layout validator over the implementations (build info + layout emitted per `foundry.toml`).
- `forge test --match-path "test/foundry/upgrade/*"` runs the upgrade rehearsal: deploy v1, populate state, `upgradeToAndCall` to a v1.1 stub, and assert all state survives.
- For the LayerZero contracts (`TargetMessenger`, `OriginMessenger`, `ONFT1155AdapterBatch`) the upgrade authority (`DEFAULT_ADMIN_ROLE`) and the OApp config authority (`owner`, gating `setPeer` / `setDelegate` / `setEnforcedOptions`) are independent tracks, set to the same address at init. Keep them unified — do not rotate one without the other.

> The production deployment runs these Foundry scripts via outbe-deploy's `deploy-intex.yml` (`deploy` = bootstrap + wire, `upgrade` = swap UUPS implementations in place).

## Other Tasks

Beyond the runbooks, the repo registers several task families. Run `yarn hardhat <task> --help` for options.

- **Wiring** (`tasks/cd/wire.ts`): `auction-wire`, `escrow-wire`, `bnb-bridge-wire`, `outbe-bridge-wire`, `onft-batch-adapter-wire`, `intex-factory-assert-relayer-role` — the post-deploy wiring sequence.
- **LayerZero** (`lz:*`): `lz:set-peer`, `lz:set-uln-config`, `lz:set-enforced-options`, `lz:grant-bridge-role`, `lz:check-peer`, `lz:quote-send`, `lz:manual-deliver`, `lz:clear-stuck-nonces`, `lz:onft1155:send`. ULN config / enforced options use `config/layerzero*.config.ts`.
- **Helpers**: `generate-commit-hash` (prefer `--series` so the derived `auctionId` matches what Desis stamps on chain) and `intex-bridge-to-outbe` (user-driven bridge of Issued Intex from BSC to Outbe).

## CI & Coverage

- **CI** ([`.github/workflows/ci-intex.yml`](../../.github/workflows/ci-intex.yml)): Solhint lint, Forge format check, Forge build, Foundry tests, Slither, Aderyn. Runs only when `contracts/intex/**` changes.
- **Coverage**: `yarn coverage:foundry` (local).

Forge is the sole Solidity compiler (`yarn compile` runs `forge build`); Hardhat is a task runner only and compiles nothing, so no dependency patch is needed.

## Static Analysis

Slither and Aderyn run on every push/PR, scanning production contracts only (`archive/`, `mocks/`, `vendor/`, and test harnesses are excluded).

```bash
mise install   # installs uv + aderyn (pinned in mise.toml)
yarn slither   # human-readable (first run installs slither via uvx)
yarn aderyn    # writes report.md
yarn analyze   # both
```

Slither uploads SARIF to GitHub code scanning and gates merges on high-severity findings (`--fail-high`); Aderyn uploads `report.md`. Config: [`slither.config.json`](./slither.config.json), [`aderyn.toml`](./aderyn.toml).

## Package Publishing

Contract ABIs and deployment addresses are published as `@outbe/intex-contracts` to GitHub Packages (`.github/workflows/publish.yml`). It triggers only via `workflow_call` from the Deploy workflow when **Publish package with addresses after deploy** is enabled — no manual or release trigger.

Contents: `dist/abi/*.json`, `dist/addresses/*.json` (per network), and `dist/index.js` + `index.d.ts` (ABI exports and `loadAddresses(network)`).

```typescript
import { IntexNFT1155ABI, DesisABI, IntexFactoryABI, loadAddresses } from '@outbe/intex-contracts';

const { contracts } = await loadAddresses('bscTestnet');
// contracts.IntexAuction, contracts.EscrowAdapter, contracts.IntexNFT1155, ...

const { contracts: outbe } = await loadAddresses('outbeTestnet');
// outbe.Desis, outbe.IntexFactory, outbe.IntexNFT1155, ...
```

Versioning: if `publish_version` is empty the latest published patch is incremented; set it to pin a version (auto-increments patch if it already exists). Addresses from multiple networks accumulate in the same package. To start fresh, run `publish.yml` manually with **clean: true** first.

## CD (Continuous Deployment)

Production deploy + wiring runs from outbe-deploy's `deploy-intex.yml` workflow against this repo. Inputs: `environment`, `USDT0_OFT_TOKEN` (payment-token wiring), `mode`.

- **`mode: deploy`** — checks out this repo, runs `forge script deploy/DeployBsc.s.sol` then `deploy/DeployOutbe.s.sol` (idempotent CREATE3 — addresses already present are skipped), regenerates `abi-export`, then runs the wiring sequence below.
- **`mode: upgrade`** — swaps UUPS implementations in place via `deploy/UpgradeIntex.s.sol`, keeping proxy addresses.

### Wiring sequence

Run after deploy and idempotent (each task reads current state and skips if already wired):

- **auction-wire** / **escrow-wire** — wire IntexAuction ↔ EscrowAdapter (+ TheCompact, Vault, payment token) on BNB.
- **bnb-bridge-wire** — wire TargetMessenger → IntexAuction, IntexNFT1155, EscrowAdapter, ONFT1155AdapterBatch.
- **outbe-bridge-wire** — wire OriginMessenger → Desis + IntexFactory (Outbe precompiles).
- **onft-batch-adapter-wire** + **lz:grant-bridge-role** — grant RELAYER_ROLE / SYSTEM_RELAYER_ROLE to the messengers and ONFT adapters; **intex-factory-assert-relayer-role** asserts IntexFactory holds RELAYER_ROLE on IntexNFT1155.
- **lz:set-peer** (bidirectional), **lz:set-uln-config**, **lz:set-enforced-options** — configure cross-chain peers between TargetMessenger (BNB) ↔ OriginMessenger (Outbe) and the ONFT adapters. ULN config / enforced options use `config/layerzero*.config.ts`.

### LayerZero EIDs

| Network | Endpoint ID |
|---------|-------------|
| bscTestnet | 40102 |
| bsc (mainnet) | 30102 |
| outbeTestnet | 40812 |
| outbeTestnetNew | 40912 |
| outbeDevnet | 40712 |
| outbePrivnet | 40512 |

### GitHub Environments

Create these under GitHub Settings → Environments:

| Environment | Chain ID | Required Secrets |
|-------------|----------|------------------|
| bscTestnet | 97 | `DEPLOYER_PRIVATE_KEY`, `RPC_URL`, `BSCSCAN_API_KEY` |
| bsc | 56 | `DEPLOYER_PRIVATE_KEY`, `RPC_URL`, `BSCSCAN_API_KEY` |
| outbeTestnet | 512215 | `DEPLOYER_PRIVATE_KEY`, `RPC_URL` |
| outbeTestnetNew | 54322345 | `DEPLOYER_PRIVATE_KEY`, `RPC_URL` |
| outbeDevnet | 424242 | `DEPLOYER_PRIVATE_KEY`, `RPC_URL` |
| outbePrivnet | 512512 | `DEPLOYER_PRIVATE_KEY`, `RPC_URL` |

Required variable per environment: `DEPLOYER_ADDRESS` (derived from `DEPLOYER_PRIVATE_KEY`). Bridge `RELAYER_ROLE` is granted to the adapters during wiring, so no separate bridger address is configured.

## Networks

| Network | Chain ID | LZ EID | RPC |
|---------|----------|--------|-----|
| bscTestnet | 97 | 40102 | `BSC_TESTNET_RPC_URL` |
| bsc | 56 | 30102 | `BSC_MAINNET_RPC_URL` |
| outbeTestnet | 512215 | 40812 | https://eth.testnet.outbe.net |
| outbeTestnetNew | 54322345 | 40912 | https://rpc.testnet.outbe.net |
| outbeDevnet | 424242 | 40712 | https://eth.d.outbe.net |
| outbePrivnet | 512512 | 40512 | https://eth.p.outbe.net |

## Environment Variables

Set in a `.env` file (see `.env.example`):

```env
OUTBE_RPC_URL=https://eth.testnet.outbe.net
OUTBE_PRIVATE_KEY=0x...
BSC_TESTNET_RPC_URL=https://bsc-testnet.publicnode.com
BSC_TESTNET_PRIVATE_KEY=0x...
BSC_MAINNET_RPC_URL=https://bsc-dataseed1.binance.org
BSC_MAINNET_PRIVATE_KEY=0x...
ETHERSCAN_API_KEY=...
```

`OUTBE_RPC_URL` selects the Outbe RPC for scripts/tasks that read `process.env` directly (the runbooks and helpers). Per-contract address overrides use `DEMO_ADDR_<CONTRACT>`.

## Project Structure

Contracts are split by deployment side (`origin` = Outbe, `target` = BNB, `shared` = both):

```
src/
├── origin/           # Deployed on Outbe Chain
│   ├── OriginMessenger.sol
│   └── interfaces/   # IDesis, IOriginMessenger   (Desis / IntexFactory are Outbe precompiles)
├── target/           # Deployed on BNB Chain
│   ├── IntexAuction.sol, EscrowAdapter.sol, TargetMessenger.sol
│   └── interfaces/   # IEscrowAdapter, IIntexAuction, ITargetMessenger
├── shared/           # Same source deployed on both chains
│   ├── IntexNFT1155.sol, ONFT1155Adapter.sol, ONFT1155AdapterBatch.sol
│   ├── interfaces/   # IERC1155Bridgeable, IIntexNFT1155, IONFT1155Adapter, IONFT1155AdapterBatch
│   └── libs/         # BridgeMsgCodec, LzGasEstimator, ONFT1155BatchMsgCodec, ONFT1155MsgCodec
├── factory/          # Create3Factory.sol
└── vendor/           # Third-party: outbe-vault (IVaultProvider), the-compact, solady (CREATE3)

scripts/
├── runbook/          # Runbook helpers: auction runtime, bids, generateCommitHash, bridgeToOutbe, harness/
├── shared/           # auctionId, layerzero, taskUtils, types, parseArgs, abi
└── cd/               # extract-abi (forge out/ → abi-export)

tasks/
├── runbook/          # auction phases, qualified (bridge), generateCommitHash, harness self-test
├── layerzero/        # bridge utils, nonce clear, ONFT1155 transfer
└── cd/               # contract wiring (wire.ts)

test/
├── foundry/          # Forge tests (+ upgrade/, cross-chain/)
└── mocks/            # Test-only Solidity fixtures
```

## Notes

- **Series format**: `yyyymmdd` (e.g. `20260526`); lex-sortable equals chronological. The series id is also the report run id for runbooks.
- **Auction schedule**: `Desis.sendAuctionStageStart` takes `clearingTimestamp` + `revealWindow` + `issuanceWindow` in the `AuctionConfig`; `commitEnd`/`revealEnd`/`issuanceEnd` are derived from them. The runbook defaults clearing to ~2h out.
- **Escrow**: bidders must approve EscrowAdapter before revealing; escrow is locked at reveal and finalized at clearing.
- **LZ fees**: runbook sends quote the fee on-chain per send (no `--value` flag); excess on the messenger sends is refunded to the runner, and excess retained on Desis is sweepable.
