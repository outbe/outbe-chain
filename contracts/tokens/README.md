# Outbe ERC-7786 Token Bridge

This package bridges the project token pairs through the ERC-7786 bridge hub and ERC-7802 mint/burn tokens.

## Model

- `ERC7786TokenBridge` is the local bridge endpoint used by users.
- Canonical-token sides use `LockUnlock`: `send()` pulls ERC20 tokens into bridge custody and inbound messages release them.
- Synthetic-token sides use `BurnMint`: `send()` calls ERC-7802 `crosschainBurn`, and inbound messages call `crosschainMint`.
- Remote bridge contracts are configured with ERC-7930 interoperable addresses via `setRemoteBridge`.

## Routes

- USDT: external-chain canonical `USDT` + lock bridge ↔ Outbe `USDT0` ERC-7802 token + Outbe mint/burn bridge.
- WCOEN: Outbe canonical `WCOEN` + Outbe lock bridge ↔ external-chain synthetic `WCOEN` ERC-7802 token + mint/burn bridge.
- `USDT0` and synthetic `WCOEN` are ERC-7802 bridgeable ERC20s.

### Networks

The external side of both routes is whichever chain `BSC_RPC` / `BSC_CHAIN_ID` and the `BSC_*` addresses point at —
BNB testnet (`97`) or Sepolia (`11155111`). No chain id is hardcoded in the scripts, so adding a network is an env
change only; keep one deployments file per network (`deployments/usdt0.bsc.env`, `deployments/usdt0.sepolia.env`) as
token and bridge addresses differ per chain.

For Sepolia, run the same commands with:

```bash
export BSC_RPC="$SEPOLIA_RPC"
export BSC_CHAIN_ID=11155111
```

Two guards follow that declaration:

- the owner of the token and token bridge must be a contract (Safe/multisig) on every declared chain
  (`BSC_CHAIN_ID`, `OUTBE_CHAIN_ID`), unless `ALLOW_EOA_OWNER=true`;
- the mock `USDT` is only deployed when the connected chain equals `BSC_CHAIN_ID` and `BSC_USDT_TOKEN` is unset —
  a wrong `--rpc-url` reverts instead of deploying a fake token.

## Scripts

Use `script/usdt0/USDT0Deploy.s.sol:USDT0Deploy` for USDT0 and `script/wcoen/WCOENDeploy.s.sol:WCOENDeploy` for WCOEN.

Common required environment:

- `PRIVATE_KEY`
- `DEPLOYER_ADDRESS` — broadcaster/signer address derived from `PRIVATE_KEY`
- `OWNER_ADDRESS` — owner for the ERC-7802 token and token bridge; use a pre-deployed Safe/multisig on guarded testnet chains
- `ALLOW_EOA_OWNER` — optional emergency override; set to `true` to allow an EOA owner on a guarded chain
- `BRIDGE_ADDRESS` — local ERC-7786 bridge hub facade
- `BSC_CHAIN_ID`
- `OUTBE_CHAIN_ID`

USDT route outputs/inputs:

- `BSC_USDT_TOKEN`
- `BSC_USDT_BRIDGE`
- `OUTBE_USDT0_TOKEN`
- `OUTBE_USDT0_BRIDGE`

WCOEN route outputs/inputs:

- `OUTBE_WCOEN_TOKEN`
- `OUTBE_WCOEN_BRIDGE`
- `BSC_WCOEN_TOKEN`
- `BSC_WCOEN_BRIDGE`

Optional deployment inputs:

- `TOKEN_NAME`, `TOKEN_SYMBOL`, `TOKEN_DECIMALS`
- `TOKEN_CREATE2_SALT`
- `TOKEN_BRIDGE_CREATE2_SALT`
- `INITIAL_MINT_AMOUNT` for the USDT dev token
- `WCOEN_TOKEN_CREATE2_SALT`
- `WCOEN_BRIDGE_CREATE2_SALT`

### Pure Forge USDT0 Flow

Start from `contracts/tokens` and load the shared environment. The deploy scripts
expect `PRIVATE_KEY`; this repo's `.env` may use `DEPLOYER_PK`, so export both.
If `BSC_RPC` points to BSC testnet (`prebsc`), use `BSC_CHAIN_ID=97`.

```bash
cd /c/Users/USER/Desktop/projects/outbe-chain/contracts/tokens

set -a
source .env
set +a

export PRIVATE_KEY="$DEPLOYER_PK"
export DEPLOYER_ADDRESS="$(cast wallet address --private-key "$PRIVATE_KEY")"
export BSC_CHAIN_ID=97
export OWNER_ADDRESS="$SAFE_ADDRESS"
```

On the configured `BSC_CHAIN_ID` and `OUTBE_CHAIN_ID`,
`OWNER_ADDRESS` must already be a deployed contract. This keeps the mint-trust
root behind a Safe/multisig while `PRIVATE_KEY` remains only the broadcaster.
Set `OWNER_ADDRESS=$DEPLOYER_ADDRESS` only for local/dev chains that are not
guarded by the deploy scripts.

For a temporary deployment where a Safe is not available yet, set
`ALLOW_EOA_OWNER=true`. This explicitly bypasses the guarded-chain contract-owner
check and leaves the deployer EOA in control of minting and bridge configuration.
Keep the override unset or `false` for normal deployments. Because the owner is
part of the CREATE2 initialization code, redeploying later with a Safe produces
different token and bridge addresses.

Deploy source-side BSC testnet contracts. If `BSC_USDT_TOKEN` is not set, this
deploys the mintable mock `USDT`; it also deploys the source `ERC7786TokenBridge`
in `LockUnlock` mode. Copy the printed `BSC_USDT_TOKEN` and `BSC_USDT_BRIDGE`
values into `deployments/usdt0.env`.

```bash
forge script script/usdt0/USDT0Deploy.s.sol:USDT0Deploy \
  --sig "deploySource()" \
  --rpc-url "$BSC_RPC" \
  --broadcast \
  --priority-gas-price 100000000
```

Deploy target-side Outbe contracts. This deploys `USDT0` and the target
`ERC7786TokenBridge` in `BurnMint` mode. Copy the printed `OUTBE_USDT0_TOKEN`
and `OUTBE_USDT0_BRIDGE` values into `deployments/usdt0.env`.

```bash
forge script script/usdt0/USDT0Deploy.s.sol:USDT0Deploy \
  --sig "deployTarget()" \
  --rpc-url "$OUTBE_RPC" \
  --broadcast \
  --priority-gas-price 100000000
```

Reload the deployed addresses:

```bash
set -a
source deployments/usdt0.env
set +a
```

Configure both remotes. The source bridge stores the Outbe remote under
`OUTBE_CHAIN_ID`; the target bridge stores the BSC testnet remote under
`BSC_CHAIN_ID`.

If `OWNER_ADDRESS` is a Safe/multisig, these scripts will not broadcast the
owner-only calls directly. They print the Safe transaction `to`, `value`, and
`data`; submit those through the owner Safe, then re-run the verification calls.
If the owner is still an EOA on a guarded testnet chain, the scripts revert
before configuration.

```bash
forge script script/usdt0/USDT0Deploy.s.sol:USDT0Deploy \
  --sig "configureSourceRemote()" \
  --rpc-url "$BSC_RPC" \
  --broadcast \
  --priority-gas-price 100000000

forge script script/usdt0/USDT0Deploy.s.sol:USDT0Deploy \
  --sig "configureTargetRemote()" \
  --rpc-url "$OUTBE_RPC" \
  --broadcast \
  --priority-gas-price 100000000
```

Verify configuration:

```bash
cast call $BSC_USDT_BRIDGE "remoteBridges(uint32)(bytes)" $OUTBE_CHAIN_ID --rpc-url "$BSC_RPC"
cast call $OUTBE_USDT0_BRIDGE "remoteBridges(uint32)(bytes)" $BSC_CHAIN_ID --rpc-url "$OUTBE_RPC"
```

Mint test source USDT on BSC testnet if needed:

```bash
cast send $BSC_USDT_TOKEN "mint(address,uint256)" $DEPLOYER_ADDRESS 100000000 \
  --private-key "$PRIVATE_KEY" \
  --rpc-url "$BSC_RPC" \
  --priority-gas-price 100000000
```

Send from BSC testnet to Outbe. `SEND_AMOUNT_LD` uses local decimals; USDT uses
6 decimals, so `1000000` is `1 USDT`.

```bash
export RECIPIENT="$DEPLOYER_ADDRESS"
export SEND_AMOUNT_LD=1000000

forge script script/usdt0/SendSourceToTarget.s.sol \
  --rpc-url "$BSC_RPC" \
  --broadcast \
  --priority-gas-price 100000000
```

Check the Outbe USDT0 balance:

```bash
cast call $OUTBE_USDT0_TOKEN "balanceOf(address)(uint256)" $DEPLOYER_ADDRESS --rpc-url "$OUTBE_RPC"
```

Send back from Outbe to BSC testnet:

```bash
forge script script/usdt0/SendTargetToSource.s.sol \
  --rpc-url "$OUTBE_RPC" \
  --broadcast \
  --priority-gas-price 100000000
```

### Short Command Reference

USDT0:

```bash
forge script script/usdt0/USDT0Deploy.s.sol:USDT0Deploy --sig "deploySource()" --rpc-url "$BSC_RPC" --broadcast
forge script script/usdt0/USDT0Deploy.s.sol:USDT0Deploy --sig "deployTarget()" --rpc-url "$OUTBE_RPC" --broadcast
forge script script/usdt0/USDT0Deploy.s.sol:USDT0Deploy --sig "configureSourceRemote()" --rpc-url "$BSC_RPC" --broadcast
forge script script/usdt0/USDT0Deploy.s.sol:USDT0Deploy --sig "configureTargetRemote()" --rpc-url "$OUTBE_RPC" --broadcast
```

WCOEN:

```bash
forge script script/wcoen/WCOENDeploy.s.sol:WCOENDeploy --sig "deploySource()" --rpc-url "$OUTBE_RPC" --broadcast
forge script script/wcoen/WCOENDeploy.s.sol:WCOENDeploy --sig "deployTarget()" --rpc-url "$BSC_RPC" --broadcast
forge script script/wcoen/WCOENDeploy.s.sol:WCOENDeploy --sig "configureSourceRemote()" --rpc-url "$OUTBE_RPC" --broadcast
forge script script/wcoen/WCOENDeploy.s.sol:WCOENDeploy --sig "configureTargetRemote()" --rpc-url "$BSC_RPC" --broadcast
```

Send examples:

```bash
forge script script/usdt0/SendSourceToTarget.s.sol --rpc-url "$BSC_RPC" --broadcast
forge script script/usdt0/SendTargetToSource.s.sol --rpc-url "$OUTBE_RPC" --broadcast
forge script script/wcoen/SendSourceToTarget.s.sol --rpc-url "$OUTBE_RPC" --broadcast
forge script script/wcoen/SendTargetToSource.s.sol --rpc-url "$BSC_RPC" --broadcast
```

Lock/unlock sends approve the local bridge first. Burn/mint sends do not require token approval because the local bridge is the authorized ERC-7802 token bridge.

## References

- EIP-7802: https://eips.ethereum.org/EIPS/eip-7802
- ERC-7786 / ERC-7930 interfaces are provided by OpenZeppelin Contracts.
