# Outbe ERC-7786 Token Bridge

This package bridges the project token pairs through the ERC-7786 bridge hub and ERC-7802 mint/burn tokens.

## Model

- `ERC7786TokenBridge` is the local bridge endpoint used by users.
- Canonical-token sides use `LockUnlock`: `send()` pulls ERC20 tokens into bridge custody and inbound messages release them.
- Synthetic-token sides use `BurnMint`: `send()` calls ERC-7802 `crosschainBurn`, and inbound messages call `crosschainMint`.
- Remote bridge contracts are configured with ERC-7930 interoperable addresses via `setRemoteBridge`.

## Routes

- USDT: external-chain canonical `USDT` + lock bridge ↔ Outbe ERC-7802 synthetic + Outbe mint/burn bridge.
- WCOEN: Outbe canonical `WCOEN` + Outbe lock bridge ↔ external-chain synthetic `WCOEN` ERC-7802 token + mint/burn bridge.
- Both synthetics are ERC-7802 bridgeable ERC20s.

### Outbe and the external chain

Every route runs between Outbe and one external EVM chain. The env names say exactly that: `OUTBE_*` is always Outbe,
`EXTERNAL_*` is whichever external chain this deployment targets — BNB testnet (`97`), Sepolia (`11155111`), anvil.
No chain id and no network name is hardcoded in the scripts, so pointing a route at another network is an env change
only:

```bash
export EXTERNAL_RPC=https://ethereum-sepolia-rpc.publicnode.com
export EXTERNAL_CHAIN_ID=11155111
```

Which side holds the canonical token differs per route — that is what `deploySource()` / `deployTarget()` refer to:

| | canonical + lock bridge | ERC-7802 synthetic + mint/burn bridge |
|---|---|---|
| usdt | external chain | Outbe |
| wcoen | Outbe | external chain |

Token and bridge addresses differ per external chain, so keep one env file per network —
`deployments/usdt.bsc.env`, `deployments/usdt.sepolia.env` — and source the one you are deploying against.

Two guards follow from that declaration:

- the owner of the token and token bridge must be a contract (Safe/multisig) on both declared chains
  (`EXTERNAL_CHAIN_ID`, `OUTBE_CHAIN_ID`), unless `ALLOW_EOA_OWNER=true`;
- the mock `USDT` is only deployed when the connected chain equals `EXTERNAL_CHAIN_ID` and `EXTERNAL_USDT_TOKEN` is
  unset — a wrong `--rpc-url` reverts instead of deploying a fake token onto the wrong network.

One Outbe-side synthetic can serve several external chains at once: `remoteBridges` is keyed by chain id, so running
`configureTargetRemote()` once per external network adds each of them without overwriting the previous one. The
CREATE2 addresses on Outbe do not depend on the external chain, so `deployTarget()` is a no-op on the second run.

## Scripts

Use `script/usdt/USDTDeploy.s.sol:USDTDeploy` for USDT and `script/wcoen/WCOENDeploy.s.sol:WCOENDeploy` for WCOEN.

Common required environment:

- `PRIVATE_KEY`
- `DEPLOYER_ADDRESS` — broadcaster/signer address derived from `PRIVATE_KEY`
- `OWNER_ADDRESS` — owner for the ERC-7802 token and token bridge; use a pre-deployed Safe/multisig on guarded testnet chains
- `ALLOW_EOA_OWNER` — optional emergency override; set to `true` to allow an EOA owner on a guarded chain
- `BRIDGE_ADDRESS` — local ERC-7786 bridge hub facade
- `EXTERNAL_CHAIN_ID`
- `OUTBE_CHAIN_ID`

USDT route outputs/inputs:

- `EXTERNAL_USDT_TOKEN`
- `EXTERNAL_USDT_BRIDGE`
- `OUTBE_USDT_TOKEN`
- `OUTBE_USDT_BRIDGE`

WCOEN route outputs/inputs:

- `OUTBE_WCOEN_TOKEN`
- `OUTBE_WCOEN_BRIDGE`
- `EXTERNAL_WCOEN_TOKEN`
- `EXTERNAL_WCOEN_BRIDGE`

Optional deployment inputs:

- `TOKEN_NAME`, `TOKEN_SYMBOL`, `TOKEN_DECIMALS`
- `TOKEN_CREATE2_SALT`
- `TOKEN_BRIDGE_CREATE2_SALT`
- `INITIAL_MINT_AMOUNT` for the USDT dev token
- `WCOEN_TOKEN_CREATE2_SALT`
- `WCOEN_BRIDGE_CREATE2_SALT`

### Pure Forge USDT Flow

Start from `contracts/tokens` and load the shared environment. The deploy scripts
expect `PRIVATE_KEY`; this repo's `.env` may use `DEPLOYER_PK`, so export both.
Point `EXTERNAL_RPC` and `EXTERNAL_CHAIN_ID` at the external chain you are deploying against.

```bash
cd /c/Users/USER/Desktop/projects/outbe-chain/contracts/tokens

set -a
source .env
set +a

export PRIVATE_KEY="$DEPLOYER_PK"
export DEPLOYER_ADDRESS="$(cast wallet address --private-key "$PRIVATE_KEY")"
export EXTERNAL_CHAIN_ID=97          # or 11155111 for Sepolia
export OUTBE_CHAIN_ID=54322345
export OWNER_ADDRESS="$SAFE_ADDRESS"
```

On the configured `EXTERNAL_CHAIN_ID` and `OUTBE_CHAIN_ID`,
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

Deploy the external-chain contracts. If `EXTERNAL_USDT_TOKEN` is not set, this
deploys the mintable mock `USDT`; it also deploys the `ERC7786TokenBridge`
in `LockUnlock` mode. Copy the printed `EXTERNAL_USDT_TOKEN` and `EXTERNAL_USDT_BRIDGE`
values into `deployments/usdt.env`.

```bash
forge script script/usdt/USDTDeploy.s.sol:USDTDeploy \
  --sig "deploySource()" \
  --rpc-url "$EXTERNAL_RPC" \
  --broadcast \
  --priority-gas-price 100000000
```

Deploy the Outbe contracts. This deploys the synthetic USDT and the
`ERC7786TokenBridge` in `BurnMint` mode. Copy the printed `OUTBE_USDT_TOKEN`
and `OUTBE_USDT_BRIDGE` values into `deployments/usdt.env`.

```bash
forge script script/usdt/USDTDeploy.s.sol:USDTDeploy \
  --sig "deployTarget()" \
  --rpc-url "$OUTBE_RPC" \
  --broadcast \
  --priority-gas-price 100000000
```

Reload the deployed addresses:

```bash
set -a
source deployments/usdt.env
set +a
```

Configure both remotes. The external-chain bridge stores the Outbe remote under `OUTBE_CHAIN_ID`;
the Outbe bridge stores the external remote under `EXTERNAL_CHAIN_ID`.

If `OWNER_ADDRESS` is a Safe/multisig, these scripts will not broadcast the
owner-only calls directly. They print the Safe transaction `to`, `value`, and
`data`; submit those through the owner Safe, then re-run the verification calls.
If the owner is still an EOA on a guarded testnet chain, the scripts revert
before configuration.

```bash
forge script script/usdt/USDTDeploy.s.sol:USDTDeploy \
  --sig "configureSourceRemote()" \
  --rpc-url "$EXTERNAL_RPC" \
  --broadcast \
  --priority-gas-price 100000000

forge script script/usdt/USDTDeploy.s.sol:USDTDeploy \
  --sig "configureTargetRemote()" \
  --rpc-url "$OUTBE_RPC" \
  --broadcast \
  --priority-gas-price 100000000
```

Verify configuration:

```bash
cast call $EXTERNAL_USDT_BRIDGE "remoteBridges(uint32)(bytes)" $OUTBE_CHAIN_ID --rpc-url "$EXTERNAL_RPC"
cast call $OUTBE_USDT_BRIDGE "remoteBridges(uint32)(bytes)" $EXTERNAL_CHAIN_ID --rpc-url "$OUTBE_RPC"
```

Mint test USDT on the external chain if needed:

```bash
cast send $EXTERNAL_USDT_TOKEN "mint(address,uint256)" $DEPLOYER_ADDRESS 100000000 \
  --private-key "$PRIVATE_KEY" \
  --rpc-url "$EXTERNAL_RPC" \
  --priority-gas-price 100000000
```

Send from the external chain to Outbe. `SEND_AMOUNT_LD` uses local decimals; USDT uses
6 decimals, so `1000000` is `1 USDT`.

```bash
export RECIPIENT="$DEPLOYER_ADDRESS"
export SEND_AMOUNT_LD=1000000

forge script script/usdt/SendSourceToTarget.s.sol \
  --rpc-url "$EXTERNAL_RPC" \
  --broadcast \
  --priority-gas-price 100000000
```

Check the Outbe synthetic USDT balance:

```bash
cast call $OUTBE_USDT_TOKEN "balanceOf(address)(uint256)" $DEPLOYER_ADDRESS --rpc-url "$OUTBE_RPC"
```

Send back from Outbe to the external chain:

```bash
forge script script/usdt/SendTargetToSource.s.sol \
  --rpc-url "$OUTBE_RPC" \
  --broadcast \
  --priority-gas-price 100000000
```

### Short Command Reference

USDT:

```bash
forge script script/usdt/USDTDeploy.s.sol:USDTDeploy --sig "deploySource()" --rpc-url "$EXTERNAL_RPC" --broadcast
forge script script/usdt/USDTDeploy.s.sol:USDTDeploy --sig "deployTarget()" --rpc-url "$OUTBE_RPC" --broadcast
forge script script/usdt/USDTDeploy.s.sol:USDTDeploy --sig "configureSourceRemote()" --rpc-url "$EXTERNAL_RPC" --broadcast
forge script script/usdt/USDTDeploy.s.sol:USDTDeploy --sig "configureTargetRemote()" --rpc-url "$OUTBE_RPC" --broadcast
```

WCOEN:

```bash
forge script script/wcoen/WCOENDeploy.s.sol:WCOENDeploy --sig "deploySource()" --rpc-url "$OUTBE_RPC" --broadcast
forge script script/wcoen/WCOENDeploy.s.sol:WCOENDeploy --sig "deployTarget()" --rpc-url "$EXTERNAL_RPC" --broadcast
forge script script/wcoen/WCOENDeploy.s.sol:WCOENDeploy --sig "configureSourceRemote()" --rpc-url "$OUTBE_RPC" --broadcast
forge script script/wcoen/WCOENDeploy.s.sol:WCOENDeploy --sig "configureTargetRemote()" --rpc-url "$EXTERNAL_RPC" --broadcast
```

Send examples:

```bash
forge script script/usdt/SendSourceToTarget.s.sol --rpc-url "$EXTERNAL_RPC" --broadcast
forge script script/usdt/SendTargetToSource.s.sol --rpc-url "$OUTBE_RPC" --broadcast
forge script script/wcoen/SendSourceToTarget.s.sol --rpc-url "$OUTBE_RPC" --broadcast
forge script script/wcoen/SendTargetToSource.s.sol --rpc-url "$EXTERNAL_RPC" --broadcast
```

Lock/unlock sends approve the local bridge first. Burn/mint sends do not require token approval because the local bridge is the authorized ERC-7802 token bridge.

## References

- EIP-7802: https://eips.ethereum.org/EIPS/eip-7802
- ERC-7786 / ERC-7930 interfaces are provided by OpenZeppelin Contracts.
