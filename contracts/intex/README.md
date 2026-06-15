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

## Demo Runbooks

The demo tasks are the canonical way to drive a full cross-chain run end to end. Each run writes a resumable report to `reports/<series-id>/report.{md,json}` with per-step tx hashes, LayerZero delivery proofs, and state assertions — that report is the demo artifact.

### Full cycle

```bash
yarn hardhat demo:auction:all --series-id 20260526 --outbe-network outbeTestnetNew
```

`demo:auction:all` runs all seven auction phases in order, pausing for **Enter** between each (`--pause false` runs unattended). LayerZero fees are quoted on-chain per send (no `--value` flag). Common options (defaults shown):

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
| 1 | `demo:auction:start` | Outbe → BNB: create the auction; BNB enters `CommittingBids` |
| 2 | `demo:auction:commit` | BNB: commit a sealed bid |
| 3 | `demo:auction:reveal` | Outbe → BNB: open the reveal stage; BNB enters `RevealingBids` |
| 4 | `demo:auction:reveal-bid` | BNB: approve + reveal the bid, locking escrow |
| 5 | `demo:auction:clearing` | Outbe → BNB: close reveals; Desis persists supply + issuance |
| 6 | `demo:auction:relay` | BNB → Outbe: relay the bids. OriginMessenger auto-fires `clearAuction`, sending AUCTION_RESULT + issuance + refund back to BNB |
| 7 | `demo:auction:verify` | BNB: confirm the series minted on `IntexNFT1155` |

Run `yarn hardhat <task> --help` for the full option list.

### Settlement lifecycle

Run after a series is `Issued` (see [Settlement / Intex Lifecycle](#settlement--intex-lifecycle)). Same `--series-id` / `--outbe-network` (defaults to `outbeTestnetNew`) / `--bnb-network` options; LZ fees auto-quoted:

- `demo:settlement:mark-qualified` — `Issued → Qualified`, signalled to BNB.
- `demo:settlement:mark-called` — `Qualified → Called`, signalled to BNB.
- `demo:settlement:settle` — authorize a settler and run `IntexSettlement.settle`; adds `--holder`, `--amount`, `--settler`.
- `settlement-mine` — Phase 3: holder calls `IntexSettlement.minePromis(seriesId, amount)`, which atomically burns `amount` Settled Intex and mints `amount * promisLoadMinor` Promis to the caller.

### Harness self-test

```bash
yarn hardhat demo:harness-selftest   # writes a sample run report (smoke test)
```

### Address resolution & keys

The demo resolves contract addresses in this order: per-contract env overrides `DEMO_ADDR_<CONTRACT>` → `node_modules/@outbe/intex-contracts/dist/addresses/<network>.json` → local `deployed-addresses.json`. External addresses (PaymentToken, VaultProvider, Metadosis, TheCompact) come from `config/external-addresses.json`. So a demo can run against a fresh deploy without editing scripts.

Runner keys are read from `.env`, one per chain: `OUTBE_PRIVATE_KEY` / `OUTBE_RPC_URL` on Outbe, `BSC_TESTNET_PRIVATE_KEY` / `BSC_TESTNET_RPC_URL` on BNB.

## Settlement / Intex Lifecycle

A series moves through `Issued → Qualified → Called → Settled`. See [docs/nft/lifecycle.md](docs/nft/lifecycle.md) for the full state diagram and rationale.

- **Issued** — auction clearing creates the series and mints Issued Intex to bidders. Tokens are tradable/bridgeable; relayer crosschainMint/crosschainBurn and voluntary settle are rejected.
- **Qualified** — `markQualified` flips the series once qualification conditions are met. Holders can bridge to Outbe and voluntarily `settle`.
- **Called** — `markCalled` (cross-chain) sweeps holder balances to Outbe and arms a `callPeriod` deadline within which holders must settle.
- **Settled** — `IntexSettlement.settle` burns Issued Intex and mints a soulbound `settledTokenId` (1:1). `Promis.minePromis` later burns Settled Intex to crosschainMint Promis.

`expireSeries` is an action, not a state: it burns remaining Issued tokens and emits `SeriesExpired`; Settled tokens are unaffected.

## Vault Integration

Settlement (Outbe) and post-finalization winner payouts (BNB) route stablecoins through the [outbe-vault](https://github.com/outbe/outbe-vault) `VaultProvider` layer — `VaultProvider.depositLiquidity(asset, amount)` is the single entry point on both chains. The underlying `VaultV2` is gated so only the `VaultProvider` can call it.

- **Outbe-side** `IntexSettlement` deposits settler stablecoins through the provider at `settle()`.
- **BNB-side** `EscrowAdapter` deposits winner principal (the `paidAmount` split) through the provider inside `finalizeAuction()`; the refund path and the Compact lock / `forcedWithdrawal` reveal flow are unchanged.

Neither contract deposits until the vault operator calls `VaultProvider.addVault(vaultV2)` **and** `addLiquiditySource(<contract>, <slot>)` on each chain — fail-loud reverts (`ReserveVaultNotConfigured` / `InvalidLiquiditySource`) surface a missing step.

Rationale, alternatives, deployment-order requirements, and known limitations (e.g. fee-on-transfer stablecoins are unsupported through the provider) are in [`docs/adr/0003-vault-provider-integration.md`](docs/adr/0003-vault-provider-integration.md). The vendored interface is at [`contracts/vendor/outbe-vault/interfaces/IVaultProvider.sol`](contracts/vendor/outbe-vault/interfaces/IVaultProvider.sol).

## Deployment

Every implementation contract is a UUPS proxy. The implementation holds only logic and chain-fixed immutables (LayerZero endpoint, endpoint ids, bridged token); all state lives in the proxy under ERC-7201 namespaced storage, and upgrades go through `upgradeToAndCall` without moving the proxy address.

Proxies are deployed through a CREATE3 factory ([`contracts/factory/Create3Factory.sol`](contracts/factory/Create3Factory.sol)), so a proxy address depends only on `(factory, deployer, salt)` and not on the implementation init code. Addresses therefore stay fixed across implementation iterations and full network wipes, and are identical across chains — including the LayerZero contracts, whose per-chain endpoint immutable would shift a CREATE2 address but not a CREATE3 one (this is why CREATE3 is used over plain CREATE2). The factory is deployed once per chain through the canonical CREATE2 deployer (`0x4e59…956C`) at a pinned salt, so it lands at the same address everywhere.

Deploy with the Foundry scripts in [`deploy/`](deploy/):

```bash
forge script deploy/DeployBsc.s.sol --rpc-url <bsc-rpc> --broadcast
forge script deploy/DeployOutbe.s.sol --rpc-url <outbe-rpc> --broadcast
```

Env: `DEPLOYER_PRIVATE_KEY`, `LZ_ENDPOINT`, and the remote endpoint id (`OUTBE_EID` for the BNB side, `BNB_EID` for the Outbe side). The deployer is the admin (`DEFAULT_ADMIN_ROLE`), owner / LZ delegate, and initial bridger (`RELAYER_ROLE`), so no separate admin/delegate/bridger addresses are passed. Deploys are idempotent: a contract already present at its predicted address is skipped, so a re-run resumes. Wiring (peers, escrow/compact/vault, roles) is a separate step (see [Other Tasks](#other-tasks)). Bump `SALT_VERSION` in [`deploy/BaseScript.s.sol`](deploy/BaseScript.s.sol) to move every contract to a fresh address set.

### Upgrade safety

- `yarn validate:upgrades` runs the OpenZeppelin upgrades-core storage-layout validator over the implementations (build info + layout emitted per `foundry.toml`).
- `forge test --match-path "test/foundry/upgrade/*"` runs the upgrade rehearsal: deploy v1, populate state, `upgradeToAndCall` to a v1.1 stub, and assert all state survives.
- For the LayerZero contracts (`TargetMessenger`, `OriginMessenger`, `ONFT1155AdapterBatch`) the upgrade authority (`DEFAULT_ADMIN_ROLE`) and the OApp config authority (`owner`, gating `setPeer` / `setDelegate` / `setEnforcedOptions`) are independent tracks, set to the same address at init. Keep them unified — do not rotate one without the other.

> The production deployment workflow still uses the previous Hardhat mechanism; its migration to these Foundry scripts is tracked separately.

## Other Tasks

Beyond the demo runbooks, the repo registers several task families. Run `yarn hardhat <task> --help` for options.

- **Wiring** (`tasks/cd/wire.ts`): `*-wire` and `*-grant-*-role` tasks (e.g. `desis-wire`, `outbe-bridge-wire`) used by the post-deploy workflow.
- **LayerZero** (`lz:*`): `lz:set-peer`, `lz:set-uln-config`, `lz:set-enforced-options`, `lz:grant-bridge-role`, `lz:quote-send`, `lz:manual-deliver`, `lz:clear-stuck-nonces`, `onft1155:send`. ULN config / enforced options use `config/layerzero*.config.ts`.
- **Auction / settlement** (legacy single-chain helpers): `auction-*`, `bidders-*`, `intex1155-issuance`, `settlement-*`, `qualified-full-flow`, `settlement-full-flow`.
- **Utilities**: `generate-commit-hash` (prefer `--series` so the derived `auctionId` matches what Desis stamps on chain), and `scripts/utils/deployCompact.ts` (deterministic CREATE2 deploy of The Compact).

## CI & Coverage

- **CI** ([`.github/workflows/ci-intex.yml`](../../.github/workflows/ci-intex.yml)): Solhint lint, Forge format check, compile, Foundry tests, Hardhat tests, Slither, Aderyn. Runs only when `contracts/intex/**` changes.
- **Coverage**: local scripts are still available via `yarn coverage:foundry` / `yarn coverage:hardhat`; the separate manual coverage workflow from `outbe-intex` has not been migrated into `outbe-chain` yet.
- **Dependency patch** (`.yarn/patches/@layerzerolabs-oapp-evm-*.patch`): `@layerzerolabs/oapp-evm-upgradeable` imports `@layerzerolabs/oapp-evm/contracts/*` transitively, but `oapp-evm`'s `package.json` `exports` omits `./contracts/*`, which Hardhat 3 rejects. The patch adds that export so `yarn compile` can resolve the upgradeable OApp bases. Forge is unaffected (it resolves via remappings).

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
// outbe.Desis, outbe.IntexFactory, outbe.IntexSettlement, outbe.IntexNFT1155, ...
```

Versioning: if `publish_version` is empty the latest published patch is incremented; set it to pin a version (auto-increments patch if it already exists). Addresses from multiple networks accumulate in the same package. To start fresh, run `publish.yml` manually with **clean: true** first.

## CD (Continuous Deployment)

### Release Flow

| Step | Workflow | Inputs |
|------|----------|--------|
| 1 | `deploy.yml` | scope: **bscCore**, publish: yes |
| 2 | `deploy.yml` | scope: **bscBridge**, publish: yes |
| 3 | `deploy.yml` | scope: **outbeMocks**, publish: yes |
| 4 | `deploy.yml` | scope: **outbeCore**, publish: yes |
| 5 | `deploy.yml` | scope: **outbeBridge**, publish: yes |
| 6 | `post-deploy.yml` | action: **all** |
| 7 | `lz-onft-peers.yml` | contract_type: **all** |
| 8 | `lz-adapters-peers.yml` | |

> 1→2 (BSC) and 3→4→5 (Outbe) are sequential within each chain; both chains can run in parallel. Step 6 only after all deploys complete. Steps 7, 8 at the very end (parallel with each other). Each `deploy.yml` step publishes `@outbe/intex-contracts` with updated addresses; `post-deploy.yml` loads from the latest published package, so always publish before wiring.

### Deploy Scopes

`.github/workflows/deploy.yml` (**Contracts Deployment**). Inputs: `environment` (target network), `scope`, `selected_contracts` (for the selective scope), and `target_chain` (bridge cross-chain EID).

- **bscCore**: IntexNFT1155, EscrowAdapter, IntexAuction (BNB side)
- **outbeCore**: IntexNFT1155, IntexSettlement, Desis, IntexFactory (Outbe side)
- **bscBridge**: ONFT1155Adapter, ONFT1155AdapterBatch, TargetMessenger (BNB side)
- **outbeBridge**: ONFT1155Adapter, ONFT1155AdapterBatch, OriginMessenger (Outbe side)
- **outbeMocks**: MockPromis, MockPromisLimit (stand-ins for the Cosmos `x/promis`/`x/promislimit` precompiles)
- **selective**: pick from `intexAuction`, `escrowAdapter`, `intexNFT1155`, `intexSettlement`, `desis`, `intexFactory`, `onft1155Adapter`, `onft1155AdapterBatch`, `targetMessenger`, `originMessenger`, `mockPromis`, `mockPromisLimit`

`environment` options: `bscTestnet`, `bsc`, `outbeTestnet`, `outbeTestnetNew`, `outbeDevnet`, `outbePrivnet`. Contract addresses are resolved dynamically from GitHub Environment vars (`DEPLOYER_ADDRESS`, `BRIDGER_ADDRESS`), the latest published package, and per-network LayerZero endpoints.

### Post-Deploy Configuration (Wiring)

`.github/workflows/post-deploy.yml` (**Contracts Wiring**). The **all** action runs every job; each targets its selected environment. All contract and external addresses are loaded from the published package automatically.

- **wire-bnb-core**: wire IntexAuction ↔ EscrowAdapter (+ TheCompact, Vault, StableToken).
- **wire-target-messenger**: wire TargetMessenger → IntexAuction, IntexNFT1155, EscrowAdapter, ONFT1155AdapterBatch; grant the relayer/system-relayer roles to TargetMessenger and the ONFT adapters.
- **wire-origin-messenger**: wire OriginMessenger → Desis + IntexFactory; wire Desis → OriginMessenger, PromisLimit, IntexFactory; grant RELAYER_ROLE to the ONFT adapters and IntexFactory on IntexNFT1155, and SYSTEM_RELAYER_ROLE to ONFT1155AdapterBatch.
- **wire-outbe-settlement**: `IntexSettlement.wire(intex, vault)`; grant SETTLEMENT_ROLE on IntexNFT1155 so it can burn Issued and mint Settled token IDs.
- **wire-outbe-promis**: wire MockPromis to IntexNFT1155 and grant PROMIS_ROLE so `Promis.minePromis` can burn Settled Intex.

Inputs: `action` (which wiring job to run, or **all**), `bnb_environment` (default `bscTestnet`), `outbe_environment` (default `outbeTestnet`).

**Note**: most wire functions support rewiring. Exception: `IntexSettlement.wire()` is one-time — reverts with `AlreadyWired`.

### LayerZero Configuration

After deploying adapters on multiple chains, configure cross-chain peers:

- `lz-onft-peers.yml` (**ONFT Adapter Peers**) — sets ONFT1155Adapter / ONFT1155AdapterBatch peers between any two chains.
- `lz-adapters-peers.yml` (**BSC-Outbe Messenger Peers**) — sets TargetMessenger (BNB) ↔ OriginMessenger (Outbe) peers.

Both run `lz:set-peer` (bidirectional), `lz:set-uln-config` (SendUln302 DVN + Executor, ReceiveUln302 DVN on the Endpoint), and `lz:set-enforced-options` (gas limits).

#### LayerZero EIDs

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

Required variables per environment: `DEPLOYER_ADDRESS` (derived from `DEPLOYER_PRIVATE_KEY`) and `BRIDGER_ADDRESS` (bridge permissions; defaults to the zero address).

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
COMPACT_FACTORY_DATA=0x...   # for scripts/utils/deployCompact.ts
```

`OUTBE_RPC_URL` selects the Outbe RPC for scripts/tasks that read `process.env` directly (the demo runbooks and helpers). Per-contract demo address overrides use `DEMO_ADDR_<CONTRACT>`.

## Project Structure

Contracts are split by deployment target (Outbe / BNB / both):

```
contracts/
├── outbe/            # Deployed on Outbe Chain
│   ├── Desis.sol, IntexFactory.sol, IntexSettlement.sol, OriginMessenger.sol
│   ├── MockPromis.sol, MockPromisLimit.sol   # x/promis(limit) precompile stand-ins
│   └── interfaces/   # IDesis, IIntexFactory, IIntexSettlement, IOriginMessenger, IPromis, IPromisLimit
├── bnb/              # Deployed on BNB Chain
│   ├── IntexAuction.sol, EscrowAdapter.sol, TargetMessenger.sol
│   └── interfaces/
├── shared/           # Same source deployed on both chains
│   ├── IntexNFT1155.sol, ONFT1155Adapter.sol, ONFT1155AdapterBatch.sol
│   ├── interfaces/
│   └── libs/         # BridgeMsgCodec, IntexMetadata, LzGasEstimator, ONFT1155BatchMsgCodec, ONFT1155MsgCodec
└── vendor/           # Third-party: outbe-vault (IVaultProvider), the-compact

scripts/
├── demo/             # Demo-runbook helpers + harness (report, runner, lz, config)
├── auction/          # Auction lifecycle, bidders, cross-chain flow, commit-hash
├── intex/            # issuance, qualify, bridgeToOutbe, settle, mine, settlementBridge
├── cd/               # CI/CD: ABI extraction, address resolution, save-addresses
├── shared/           # auctionId, runtime, LZ helpers, wallets, parseArgs
└── utils/            # balance checks, deployCompact, sendCoen, sendOpTx

tasks/
├── demo/             # Demo runbooks: auction, settlement, harness self-test
├── auction/          # flow, stage management, bidders, cross-chain, commit-hash
├── intex/            # issuance, qualify, qualified, settlement
├── layerzero/        # bridge utils, nonce clear, ONFT1155 transfer
└── cd/               # contract wiring

test/
├── foundry/          # Forge tests (+ cross-chain/)
├── hardhat/          # Hardhat tests (+ cross-chain/)
└── mocks/            # Test-only Solidity fixtures
```

## Notes

- **Series format**: `yyyymmdd` (e.g. `20260526`); lex-sortable equals chronological. The series id is also the report run id for demo runbooks.
- **Auction schedule**: `Desis.sendAuctionStageStart` takes `clearingTimestamp` + `revealWindow` + `issuanceWindow` in the `AuctionConfig`; `commitEnd`/`revealEnd`/`issuanceEnd` are derived from them. The demo defaults clearing to ~2h out; the legacy `auction-*` flow anchors it to noon UTC of the series date.
- **Escrow**: bidders must approve EscrowAdapter before revealing; escrow is locked at reveal and finalized at clearing.
- **LZ fees**: demo sends pass `--value` as `msg.value` directly (no on-chain quote). Set it generously — excess on the messenger sends is refunded to the runner; excess retained on Desis is sweepable.
