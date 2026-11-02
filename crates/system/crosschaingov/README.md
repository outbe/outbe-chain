# CrosschainGov

`outbe-crosschaingov` is the governance owner of every multi-chain contract
(OIP-00369). On Outbe the owner is this precompile. On every other chain the
owner is its Interchain Account (ICA). It owns:

- the Hyperlane core contracts (see `outbe-hyperlanecontroller`);
- the ERC-7786 hub (`ERC7786Bridge`) and its gateway adapters (`contracts/crosschain`);
- the token bridges (`ERC7786TokenBridge`, `contracts/tokens`);
- the intent contracts: `Auction`, `SolverEscrow`, `SolverAllocator`, `Router`
  and `RouterAllocator` (`contracts/intent`).

Address: `0x000000000000000000000000000000000000EE14` (`CROSSCHAIN_GOV_ADDRESS`,
Solidity `CROSSCHAIN_GOV` in `contracts/precompiles/src/ICrosschainGov.sol`).
One precompile has one balance for remote fees, one ICA on each chain and one
vote target.

## Modules

| file | content |
|---|---|
| `precompile.rs` | `applyApproved`, `fund` and `remoteAccount`. Every other selector goes to `outbe_hyperlanecontroller::dispatch` without logic of its own |
| `owner.rs` | the owner calls `call_local` and `call_remote`. `CrosschainGov` implements `outbe_hyperlanecontroller::Owner` with them |
| `vote_target.rs` | the one vote target of `0x…EE14` and `apply_approved` |
| `actions/common.rs` | `upgradeContract` |
| `actions/crosschain.rs` | `setBridgePaused`, `setBridgeGateway`, `linkBridges` |
| `actions/tokens.rs` | `linkTokenBridges` |

`outbe-crosschaingov` depends on `outbe-hyperlanecontroller`. The Hyperlane
controller sees only its trait `Owner`. All code runs in the frame of
`0x…EE14`, so `msg.sender` of every owner call is `0x…EE14`. A call from another
precompile has a different `msg.sender` and fails the owner check of the target.

Place of a new action:

| The action changes | Place |
|---|---|
| any of our UUPS contracts in the same way | `actions/common.rs` |
| the functions of one contract family | `actions/<family>.rs` |
| the Hyperlane infrastructure | `outbe-hyperlanecontroller/src/actions.rs` |

## State

The Hyperlane controller uses slots 0-9 of the same account.

| slot | field | meaning |
|---|---|---|
| 10 | `approved_payload` | `proposal id -> keccak256(payload)` of an approved action; zero = none or applied |
| 11 | `approved_at` | `proposal id -> block of the approval` |

## Selectors

- `fund()` payable — tops up the balance that pays IGP fees for ICA dispatches.
  It is the only payable selector.
- `applyApproved(proposalId, payload)` — permissionless. Runs an action that a
  vote approved (see below).
- `remoteAccount(domain)` — the ICA of `0x…EE14` on `domain`, as the Outbe ICA
  router derives it.
- `initialize`, `sync`, `submitCheckpoint`, `setHyperlaneSigner` and the
  Hyperlane views — see `crates/system/hyperlanecontroller/README.md`.

## Owner calls

| method | effect |
|---|---|
| `call_local(to, value, data)` | a call on Outbe from `0x…EE14`. A non-zero `value` comes from the balance. Emits `LocalCallExecuted` |
| `call_remote(domain, calls[])` | one ICA message to a remote chain. Reads the ICA router from the Hyperlane controller, pays `quoteGasPayment(domain)` from the balance and reverts before the dispatch if the balance is too low. Emits `RemoteCallDispatched` |

## Vote target

`CrosschainVoteTarget` serves `0x…EE14`. Validators vote on one strict JSON
action with an `operation` field, so no proposal can carry a free `to` and
`data`. The vote target reads `operation`. `addHyperlaneChain` and
`removeHyperlaneChain` go to `outbe-hyperlanecontroller`. Every other operation
goes to `actions`. The selected module checks the payload when the proposal is
created (unknown fields and duplicate fields are rejected), and runs it on
`applyApproved`.

The tally runs where sub-calls are not available. So an approved proposal
stores only `keccak256(payload)` and the approval block. Anyone then calls
`applyApproved(proposalId, payload)` with the same bytes, and the precompile
runs the action once. An approval older than the governance voting window is
dropped with `ApprovedActionExpired`, so a stale approval cannot undo a newer
one.

A chain id is also the Hyperlane domain of that chain. Calls on the Outbe chain
id go through `call_local`. Calls on another chain id go in one `call_remote`.
A `link…` action does both: the Outbe side first, then one message with the
mirror calls. Everything runs in one checkpoint.

| `operation` | fields | calls |
|---|---|---|
| `upgradeContract` | `chainId`, `proxy`, `implementation`, `callData` | `proxy.upgradeToAndCall(implementation, callData)` |
| `setBridgePaused` | `chainId`, `bridge`, `paused` | `bridge.pause()` or `bridge.unpause()` |
| `setBridgeGateway` | `chainId`, `bridge`, `destinationChainId`, `adapter` | `bridge.setGateway(destinationChainId, adapter)`: the adapter the bridge sends through. A zero `adapter` clears the override |
| `linkBridges` | `remoteChainId`, `bridge`, `adapter`, `remoteBridge`, `remoteAdapter` (Hyperlane adapters) | on each side: `adapter.setRouterWithChain` (the other adapter), `bridge.registerRemoteBridge` (ERC-7930 of the other bridge), `bridge.setGateway(other chain, adapter)` |
| `linkTokenBridges` | `remoteChainId`, `tokenBridge`, `remoteTokenBridge` | on each side: `tokenBridge.addPeer(other chain, other token bridge)` |
| `addHyperlaneChain`, `removeHyperlaneChain` | see `outbe-hyperlanecontroller` | |

```json
{"operation":"upgradeContract","chainId":11155111,"proxy":"0x…","implementation":"0x…","callData":"0x"}
```

### `upgradeContract`

`chainId`, `proxy` and `implementation` must not be zero. An empty `callData`
is a plain upgrade. Otherwise `callData` can call a `reinitializer` of the new
implementation. The action serves every UUPS proxy that `0x…EE14` or its ICA
owns: the hub, the gateway adapters, the token bridges and the intent
contracts.

A configuration change on a contract without a typed action uses the same
operation: the current implementation and the call data of an admin function.
`upgradeToAndCall` keeps `msg.sender`, so the `onlyOwner` check passes.

### Checks for voters

The precompile does not check what the addresses are. Voters check them:

- `upgradeContract`: the implementation bytecode comes from a named commit and
  passes the upgrade-safety check in CI. An upgrade runs any code that the vote
  approves.
- `linkTokenBridges`: `token()` of both token bridges.
- `linkBridges`: the bridge and adapter addresses on both chains.

A peer link cannot be removed.

A remote action is asynchronous: `applyApproved` succeeds when the message is
dispatched, not when the remote chain runs it. If `upgradeToAndCall` reverts
on the remote chain, the proposal on Outbe stays applied. The ICA router
reports the failure on the remote chain only.

## Hand-over

Each deploy gives its contracts to `CROSSCHAIN_GOV` on Outbe and to its ICA
(`remoteAccount`) on other chains:

| Contracts | Step |
|---|---|
| Hub and gateway adapters | `contracts/crosschain/script/4_HandOver.s.sol` |
| Token bridge | `contracts/tokens/script/routes/BaseRoute.sol` |
| Intent contracts | `contracts/intent/script/5_HandOver.s.sol` |

The deployer owns the contracts while it deploys and configures them.
`KEEP_DEPLOYER_AS_OWNER=true` skips the hand-over of the crosschain and the
intent deploy.
