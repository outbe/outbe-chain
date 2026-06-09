# Outbe OFT Stablecoin Integration

This repository implements the Outbe stablecoin bridge flow using LayerZero OFT v2.

Primary design record:

- [0002-stablecoins-oft-impl.md](https://github.com/outbe/outbe-chain/blob/main/adrs/0002-stablecoins-oft-impl.md)

## Scope

The ADR defines:

- Generic ERC20 -> OFT integration pattern (`OFTAdapter` on source chain, `OFT` on Outbe)
- Mainnet coordination model for `USDT0` and `USDe`
- Testnet architecture for BSC Testnet <-> Outbe using mock USDT
- Step-by-step deployment and verification flow

## OFT Model (LayerZero v2)

- Source chain: `OFTAdapter` locks/unlocks the canonical ERC20.
- Outbe chain: `OFT` mints on inbound messages and burns on outbound messages.
- Messaging path: LayerZero Endpoint + DVN verification + Executor delivery.

This is a lock-and-mint / burn-and-unlock topology (no liquidity pool bridge required).

## Networks and EIDs

Devnet

```
SRC_EID=40102
OUTBE_EID=40512
OUTBE_DEV_EID=40712
```

Testnet

```
SRC_EID=40102
OUTBE_EID=40812
OUTBE_DEV_EID=40712
```

## Shared Outbe LayerZero Addresses

These addresses are documented in the ADR as deployed on Outbe networks and BSC Testnet:

- `EndpointV2`: `0x2915f5C5835576CC6E4bFBc002519E2B37cCABe2`

## Testnet Target Topology

Recommended path in ADR: use BSC Testnet as source side (already configured in `outbe-layerzero`).

- BSC Testnet (source/lock chain):
  - Deploy `USDT` (mintable ERC20 test token)
  - Deploy `OFTAdapter` wrapping source-side `USDT`
- Outbe Testnet (destination/mint chain):
  - Deploy `USDT0` OFT contract

## Current Contract Logic

- `USDT` (`src/USDT.sol`)
  - ERC20 with fixed `6` decimals
  - Permissionless `mint` function for testnet and local testing
- `WCOEN` (`src/WCOEN.sol`)
  - Wrapped-native style token with `deposit` and `withdraw`
  - No `mint` function; WCOEN supply is created only by wrapping native COEN through `deposit()` and burned only by `withdraw()`
- `OFTAdapter` (`src/OFTAdapter.sol`)
  - Inherits LayerZero `OFTAdapter` without extra restrictions
  - Handles lock/unlock semantics for the source ERC20
- `USDT0OFT` (`src/USDT0OFT.sol`)
  - Implements the default LayerZero OFT mint/burn behavior with constructor-configured name, symbol, and local decimals
  - Handles mint on receive and burn on send on Outbe side

## Deployment and Testing

### Main Script

Use `script/usdt0/OFTDeploy.s.sol` for the USDT0 flow and `script/wcoen/OFTDeploy.s.sol` for the WCOEN flow. Each directory also contains matching send scripts.

USDT0 script functions:

- `deploySource()` — Deploy the source-side OFTAdapter
- `predictOutbe()` — Predict the CREATE2 USDT0OFT address using `OFT_NAME`, `OFT_SYMBOL`, `OFT_DECIMALS`, and `OFT_CREATE2_SALT`
- `deployTarget()` — Deploy or reuse USDT0OFT at its CREATE2 address on Outbe using `OFT_NAME`, `OFT_SYMBOL`, and `OFT_DECIMALS`
- `configureSourcePeer()` — Set adapter peer
- `configureTargetPeer()` — Set OFT peer
- `setSourceOptions()` — Configure source enforced options
- `setTargetOptions()` — Configure Outbe enforced options
- `SendSourceToTarget.s.sol` — Send source-chain tokens from BSC to Outbe
- `SendTargetToSource.s.sol` — Send Outbe OFT tokens back to BSC

WCOEN script functions:

- `predictSource()` — Predict the CREATE2 source-side WCOEN + Outbe OFTAdapter addresses
- `deploySource()` — Deploy or reuse source-side WCOEN + Outbe OFTAdapter at their CREATE2 addresses
- `predictOutbe()` — Predict the CREATE2 WCOENOFT address using `OFT_NAME`, `OFT_SYMBOL`, `OFT_DECIMALS`, and `OFT_CREATE2_SALT`
- `deployTarget()` — Deploy or reuse WCOENOFT at its CREATE2 address
- `configureSourcePeer()` — Set adapter peer
- `configureTargetPeer()` — Set OFT peer
- `setSourceOptions()` — Configure source enforced options
- `setTargetOptions()` — Configure OFT enforced options

Single-step example:

```bash
export OFT_NAME=USDT0
export OFT_SYMBOL=USDT0
export OFT_DECIMALS=6

forge script script/usdt0/OFTDeploy.s.sol:OFTDeploy \
  --sig "deployTarget()" \
  --rpc-url "$OUTBE_RPC" \
  --broadcast
```

Predict the CREATE2 address before broadcasting:

```bash
forge script script/usdt0/OFTDeploy.s.sol:OFTDeploy \
  --sig "predictOutbe()" \
  --rpc-url "$OUTBE_RPC"
```

### Environment Variables

Required for deploy + configure:

- `PRIVATE_KEY`
- `DEPLOYER_ADDRESS`
- `SRC_RPC`
- `OUTBE_RPC`
- `SRC_EID`
- `OUTBE_EID`
- `LZ_ENDPOINT`

Required for configure + send (after deployment):

- `SOURCE_TOKEN`
- `OFT_ADAPTER`
- `OFT_TOKEN`
- `RECIPIENT`

Optional:

- `OFT_NAME` (default: `USDT0`)
- `OFT_SYMBOL` (default: `USDT0`)
- `OFT_DECIMALS` (default: `6` for USDT0 and fixed `18` for WCOEN; WCOEN deploy scripts reject any other value)
- `OFT_CREATE2_SALT` (default: `USDT0OFT` for USDT0 scripts and `WCOENOFT` for WCOEN scripts)
- `WCOEN_CREATE2_SALT` (default: `WCOEN`; WCOEN source script only)
- `WCOEN_ADAPTER_CREATE2_SALT` (default: `WCOEN_OFT_ADAPTER`; WCOEN source script only)
- `INITIAL_MINT_AMOUNT` (default: `1_000_000_000e6`; USDT0 source mock only)
- `SEND_AMOUNT_LD` (default: `1_000_000`)
- `SEND_MIN_AMOUNT_LD` (default: `0`)
- `SEND_BACK_AMOUNT_LD` (default: `1_000_000`)
- `SEND_BACK_MIN_AMOUNT_LD` (default: `0`)
- `RECEIVER_PRIVATE_KEY` (used by the flow scripts for the outbe -> source send step)

### End-to-End Flow (Manual)

```bash
# 1) Deploy source contracts (USDT + OFT adapter)
forge script script/usdt0/OFTDeploy.s.sol:OFTDeploy --sig "deploySource()" --rpc-url "$SRC_RPC" --broadcast

# 2) Deploy Outbe OFT
forge script script/usdt0/OFTDeploy.s.sol:OFTDeploy --sig "deployTarget()" --rpc-url "$OUTBE_RPC" --broadcast

# 3) Configure peers
forge script script/usdt0/OFTDeploy.s.sol:OFTDeploy --sig "configureSourcePeer()" --rpc-url "$SRC_RPC" --broadcast
forge script script/usdt0/OFTDeploy.s.sol:OFTDeploy --sig "configureTargetPeer()" --rpc-url "$OUTBE_RPC" --broadcast

# 4) Configure enforced options
forge script script/usdt0/OFTDeploy.s.sol:OFTDeploy --sig "setSourceOptions()" --rpc-url "$SRC_RPC" --broadcast
forge script script/usdt0/OFTDeploy.s.sol:OFTDeploy --sig "setTargetOptions()" --rpc-url "$OUTBE_RPC" --broadcast

# 5) Send source -> Outbe
forge script script/usdt0/SendSourceToTarget.s.sol --rpc-url "$SRC_RPC" --broadcast

# 6) Send Outbe -> source
forge script script/usdt0/SendTargetToSource.s.sol --rpc-url "$OUTBE_RPC" --broadcast
```

### GitHub Actions

`deploy-usdt-oft.yml` executes deployment + peer/options configuration (steps 1 to 4) with:

- `LZ_ENDPOINT` (single endpoint address for both chains)
- `SRC_RPC` and `OUTBE_RPC`
- `SRC_EID` and `OUTBE_EID`

## Deployment Notes

The USDT0 flow deploys a mock source token plus adapter on BSC and a mint/burn OFT on Outbe. The WCOEN flow is reversed: canonical WCOEN lives on Outbe, the Outbe adapter locks WCOEN, and BSC WCOENOFT mints/burns via LayerZero.

WCOEN source deployment is deterministic. If `WCOEN_TOKEN` is unset, `deploySource()` deploys or reuses WCOEN through the canonical CREATE2 factory using `WCOEN_CREATE2_SALT`. The Outbe `OFTAdapter` is also deployed or reused through CREATE2 using `WCOEN_ADAPTER_CREATE2_SALT`; its address stays stable as long as the token address, LayerZero endpoint, owner, adapter salt, adapter bytecode, and CREATE2 factory stay the same. Use `predictSource()` before broadcasting to print both source addresses.

If `WCOEN_TOKEN` is set, the script treats that address as the canonical WCOEN and only the adapter is CREATE2-deployed from that configured token. `deploySource()` does not wrap native COEN; fund WCOEN balances separately with `deposit()` when needed.

Target-side `WCOENOFT` is CREATE2-deployed with `OFT_CREATE2_SALT` and is reused when code already exists at the predicted address. Changing salts, constructor inputs, bytecode, or factory changes the predicted address and requires updating `.env` plus peer configuration.

## Decimal Notes

OFT uses shared decimals (default `6`) for cross-chain normalization. `OFT_DECIMALS` controls the local ERC20 decimals exposed by USDT0OFT. WCOENOFT is deployed with fixed local decimals `18` to match WCOEN. If you must preserve `18` shared decimals across chains, override `sharedDecimals()` in your OFT implementation.

## References

- Outbe LayerZero Infrastructure: https://github.com/outbe/outbe-layerzero
- USDT0 Documentation: https://docs.usdt0.to/
- USDT0 Deployments: https://docs.usdt0.to/technical-documentation/deployments
- LayerZero OFT Metadata: https://metadata.layerzero-api.com/v1/metadata/experiment/ofts/list
- LayerZero V2 OFT Standard: https://docs.layerzero.network/v2/home/protocol/contract-standards
