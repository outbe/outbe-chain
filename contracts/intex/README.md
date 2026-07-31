# Outbe Intex Contracts

Cross-chain contracts for the daily Intex auction. One origin chain (Outbe) hosts the auction engine next to the Desis and IntexFactory precompiles; every registered target chain runs a full auction venue where users commit, reveal and receive their Intexes. The origin chain is itself one of the venues, reached through the hub's loopback gateway, so adding a chain never special-cases the local one.

## Model

- The auction is global: one worldwide day = one auction across all registered target chains. Desis broadcasts the stage schedule, collects every chain's revealed bids, clears at a single uniform rate and issues winners on the chain where they bid.
- All cross-chain traffic goes through the shared ERC-7786 hub (`contracts/crosschain`). The hub is transport-agnostic — Hyperlane, LayerZero and loopback adapters live behind it — and these contracts bind only the hub address. No LayerZero endpoints, peers or DVN configuration are part of running intex.
- Payments are wCOEN on every venue: bids are escrowed in it and auction proceeds return to Outbe through the wCOEN token bridge, where IntexFactory distributes them to the day's tribute creators. The token and its bridge are external inputs supplied at deploy time.

## Contracts

Origin (Outbe):

- `origin/OriginRouter.sol` — the engine's transport endpoint. Owns the target-chain registry and the per-day snapshot frozen at stage start, broadcasts stage messages, addresses per-chain result/refund/issuance sends, receives bid batches and proceeds, and parks any failed leg for permissionless retry. Wired to the Desis (`0x…1016`) and IntexFactory (`0x…1015`) precompiles via `wire()`.

Each target chain (including Outbe as the loopback venue):

- `target/TargetRouter.sol` — per-chain orchestration: applies stage/result/issuance/refund/lifecycle messages, relays revealed bids back to the origin (batches plus a `BIDS_DONE` completeness marker), drives holder migration on call, and routes proceeds home.
- `target/IntexAuction.sol` — the commit/reveal auction state machine. The day state (green/red) and the whole schedule are final at `auctionStart`; stage flips follow that schedule on the local clock, so no cross-chain reveal signal exists.
- `target/EscrowAdapter.sol` — bid escrow and commit bonds on TheCompact; locks at commit, releases the bond at reveal, settles winners and refunds losers in wCOEN.
- `shared/IntexNFT1155.sol` + `shared/IntexNFT1155Bridge.sol` — the Intex ERC-1155 ledger (Issued/Settled per series) and its cross-chain balance carrier with park/retry recovery on both legs.
- `shared/libs/BridgeMsgCodec.sol` — the canonical message codec shared by both routers.

## Message flows

| Message | Direction | Purpose |
| --- | --- | --- |
| `AUCTION_STAGE_START` | origin → every snapshot chain | schedule, prices, day state; a red day still announces itself and cancels |
| `AUCTION_STAGE_CLEARING` | origin → every snapshot chain | close the venue and trigger the bid relay |
| `BIDS_BATCH`, `BIDS_DONE` | target → origin | revealed bids in bounded batches plus the per-chain completeness marker |
| `AUCTION_RESULT` | origin → per chain | won-bid count (zero keeps a skipped venue consistent) |
| `ISSUANCE_INSTRUCTIONS` | origin → every snapshot chain | series creation and winner mints; an empty recipient list provisions the series only |
| `REFUND_INSTRUCTIONS` | origin → per chain | loser refunds through the escrow |
| `MARK_QUALIFIED`, `MARK_CALLED` | origin → every snapshot chain | series lifecycle transitions; `MARK_CALLED` also starts holder migration to Outbe |

Proceeds travel separately as a composed wCOEN bridge transfer from each venue's escrow to the OriginRouter, which forwards them to IntexFactory per source chain for the creator-reward fan-in.

## Deploy

All contracts are UUPS proxies at CREATE3-deterministic addresses (salt `outbe-intex:<Name>:<SALT_VERSION>`, deployer-namespaced), so addresses are identical across chains and predictable before broadcast.

- `deploy/DeployOrigin.s.sol` — the origin engine only. Deploys `OriginRouter`, registers every chain from `TARGET_CHAIN_IDS` against the CREATE3-predicted `TargetRouter` address and configures the proceeds route. Env: `DEPLOYER_PRIVATE_KEY`, `BRIDGE_ADDRESS`, `TARGET_CHAIN_IDS` (comma-separated), optional `OUTBE_WCOEN_BRIDGE`/`OUTBE_WCOEN_TOKEN`.
- `deploy/DeployTarget.s.sol` — the full venue stack, uniform for every chain: NFT, NFT bridge, escrow, auction, `TargetRouter`. Peers the router with the origin and meshes the NFT bridge with every other venue. Env: `DEPLOYER_PRIVATE_KEY`, `BRIDGE_ADDRESS`, `ORIGIN_CHAIN_ID`, `TARGET_CHAIN_IDS`, optional `WCOEN_TOKEN`/`WCOEN_BRIDGE`.
- `deploy/UpgradeIntex.s.sol` — `UpgradeOrigin` / `UpgradeTarget` redeploy implementations and upgrade the existing proxies in place; proxy storage (roles, peers, pending queues) persists.
- `SALT_VERSION` overrides the CREATE3 salt for throwaway test deployments; blank keeps the production address set.

Deploys are idempotent: an already-deployed proxy is returned unchanged, so a partially failed run can simply be re-run.

## Development

```bash
yarn install --immutable
yarn compile          # forge build + ABI artifacts
yarn test             # forge test (unit, cross-chain, deploy and upgrade suites)
yarn lint             # forge lint
yarn validate:upgrades
```

Hardhat is a task runner only — compilation and tests are pure Forge. The cross-chain suite under `test/foundry/cross-chain/` carries the protocol walks: `LocalLoopback.t.sol` runs a complete origin-as-venue auction on a single chain, `OriginRouterMultiTarget.t.sol` covers the multi-venue fan-out, and the codec golden tests pin the wire format.
