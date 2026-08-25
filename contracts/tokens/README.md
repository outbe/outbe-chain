# Outbe ERC-7786 Token Bridge

This package bridges the project token pairs through the ERC-7786 bridge hub and ERC-7802 mint/burn tokens.

## Model

- `ERC7786TokenBridge` is the local bridge endpoint used by users.
- Canonical-token sides use `LockUnlock`: `send()` pulls ERC20 tokens into bridge custody and inbound messages release them.
- Synthetic-token sides use `BurnMint`: `send()` calls ERC-7802 `crosschainBurn`, and inbound messages call `crosschainMint`.
- Remote bridge contracts are configured with ERC-7930 interoperable addresses via `setRemoteBridge`.

## Routes

Every route runs between Outbe and one external EVM chain. `OUTBE_*` is always Outbe; `EXTERNAL_*` is whichever
external chain this deployment targets — BNB testnet (`97`), Sepolia (`11155111`), anvil. No chain id and no network
name is hardcoded, so pointing a route at another network is an env change only.

| route | canonical + lock bridge | ERC-7802 synthetic + mint/burn bridge |
|---|---|---|
| USDT | external chain | Outbe |
| WCOEN | Outbe | external chain |

Which side is canonical on the connected chain is derived from `OUTBE_CHAIN_ID`, so the same command runs everywhere.

## One address on every chain

Contracts are deployed through a CREATE3 factory (`CreateX`), so an address depends only on
`(factory, salt, deployer)` — never on the bytecode or the constructor arguments:

```
proxy    = CREATE2(factory, salt, keccak256(PROXY_BYTECODE))   // fixed 16-byte proxy
deployed = CREATE(proxy, nonce = 1)
```

That is what lets **one address** hold the mock `USDT` on BSC and Sepolia and the ERC-7802 synthetic on Outbe, and one
bridge address hold a `LockUnlock` bridge on one chain and a `BurnMint` bridge on another. The remote bridge therefore
shares the local bridge's address, which is why `REMOTE_CHAIN_IDS` lists chain ids only — the same list works
unchanged on every chain, and a chain can be wired before it is deployed.

Salts are `keccak256(label, CONTRACT_SALT, deployer)` over four labels: `USDT`, `USDTBridge`, `WCOEN`, `WCOENBridge`.

The property holds while all of these hold:

1. the same `CONTRACT_SALT` on every chain;
2. the same deployer key — it is part of every salt, so rotating it is a full redeploy;
3. the same factory address: either `CREATEX_ADDRESS` pinned, or unchanged compiler settings (`solc`,
   `optimizer_runs`, `via_ir`, `evm_version`, `bytecode_hash`, `cbor_metadata` all feed the factory's own address);
4. `0x4e59b4488CE4Bd6E1BdD52D4bC0EE4Bf9E1C3A55` (the deterministic CREATE2 factory) present on every chain;
5. `CANONICAL_USDT_TOKEN` / `CANONICAL_WCOEN_TOKEN` unset for that route.

If a canonical token already exists and is not ours to place — the issuer's USDT on a real network — set
`CANONICAL_USDT_TOKEN` and that route adopts it instead of deploying. Only that token's address is given up; the
bridge address stays identical everywhere, because the token only enters the bridge's constructor arguments and
CREATE3 ignores those.

It does **not** depend on the owner, `BRIDGE_ADDRESS`, the bridge mode, the token metadata or the deployer's nonce.

## Guards

- The owner of the token and token bridge must be a contract (Safe/multisig) on both declared chains
  (`EXTERNAL_CHAIN_ID`, `OUTBE_CHAIN_ID`), unless `ALLOW_EOA_OWNER=true`.
- Nothing is deployed onto a chain that is neither `EXTERNAL_CHAIN_ID` nor `OUTBE_CHAIN_ID`. A wrong `--rpc-url`
  reverts with `UndeclaredChain` — without it an unrecognised chain would count as "not Outbe", i.e. as the external
  end of every route, and a full set of contracts including the mintable USDT mock would land on it.
- When the owner is a Safe, the owner-only calls are not broadcast: the scripts print `to` / `value` / `data` for you
  to submit through the Safe. Re-running afterwards verifies the result and sends nothing.

## Deploy

One command per chain. Every step self-checks against on-chain state, so a re-run on a finished chain sends no
transactions.

`NETWORK` is an alias from `[rpc_endpoints]` in `foundry.toml`: `outbe-testnet`, `outbe-dev`, `outbe-privnet`,
`bsc-testnet`, `sepolia`, `local`.

```bash
NETWORK=bsc-testnet   mise run deploy-all
NETWORK=sepolia       mise run deploy-all
NETWORK=outbe-testnet mise run deploy-all
```

Or directly:

```bash
forge script script/DeployAll.s.sol:DeployAll --rpc-url bsc-testnet --broadcast
forge script script/DeployAll.s.sol:DeployAll --rpc-url outbe-testnet --broadcast
```

`DeployAll` runs four phases: CreateX factory → USDT route → WCOEN route → remote wiring. Individual phases are
available as `script/0_DeployCreateX.s.sol`, `script/1_DeployRoutes.s.sol`, `script/2_ConfigureRemotes.s.sol`
(`mise run deploy-createx` / `deploy-routes` / `configure-remotes`).

Script layout:

| file | role |
|---|---|
| `routes/BaseRoute.sol` | route-agnostic: guards, salts, address prediction, the deploy sequence |
| `routes/UsdtRoute.sol` | everything specific to USDT — labels, which side is canonical, metadata, dev mock |
| `routes/WcoenRoute.sol` | the same for WCOEN |
| `1_DeployRoutes.s.sol` | assembles the routes and deploys them |

Adding a token is a new file under `routes/` plus two lines in the assembler — the shared code does not change. The
salt labels in each route file are part of the CREATE3 address: editing one relocates that token everywhere.

**Simulate first.** Without `--broadcast` the scripts still print the four addresses. Run that on every chain and
confirm the addresses match before sending anything — the cheapest possible proof that the deployment is coherent.

Adding a network later: deploy it, then re-run `configure-remotes` on the existing chains so the wiring is
bidirectional. `remoteBridges` is a mapping keyed by chain id, so this is additive.

## Send

```bash
export ROUTE=usdt            # or wcoen
export DEST_CHAIN_ID=54322345
export RECIPIENT=0x...
export SEND_AMOUNT_LD=1000000    # token decimals; USDT has 6, so this is 1 USDT

NETWORK=bsc-testnet mise run send
```

The bridge address is derived from CREATE3 and the token is read off the bridge, so no address can drift out of sync
with the deployment. The approval step follows the bridge's own mode: lock/unlock pulls the token, burn/mint does not.

## Environment

See `.env.example`. Required for a deploy: `NETWORK`, `DEPLOYER_PK`, `CONTRACT_SALT`, `BRIDGE_ADDRESS`, `OUTBE_CHAIN_ID`,
`EXTERNAL_CHAIN_ID`. Optional: `OWNER_ADDRESS`, `ALLOW_EOA_OWNER`, `CREATEX_ADDRESS`, `REMOTE_CHAIN_IDS`,
`CANONICAL_USDT_TOKEN`, `CANONICAL_WCOEN_TOKEN`, `INITIAL_MINT_AMOUNT`, `INITIAL_MINT_RECIPIENT`.

Token and bridge addresses are computed, never configured — they only appear in the scripts' output.

## Verify

```bash
# same address, different code, correct role
cast code $USDT_BRIDGE --rpc-url outbe-testnet | cast keccak
cast call $USDT_BRIDGE "mode()(uint8)" --rpc-url outbe-testnet   # 1 = BurnMint
cast call $USDT_BRIDGE "mode()(uint8)" --rpc-url bsc-testnet     # 0 = LockUnlock

# wiring
cast call $USDT_TOKEN  "tokenBridge()(address)" --rpc-url outbe-testnet
cast call $USDT_BRIDGE "remoteBridges(uint32)(bytes)" 97 --rpc-url outbe-testnet
```

Because a CREATE3 address is bytecode-independent, anyone can occupy an unused salt on a permissionless factory.
Deploy the chains back to back and compare the `cast code` hash against the local artifact on each chain before
wiring; a mismatch is a stop, not a retry.

## References

- EIP-7802: https://eips.ethereum.org/EIPS/eip-7802
- ERC-7786 / ERC-7930 interfaces are provided by OpenZeppelin Contracts.
