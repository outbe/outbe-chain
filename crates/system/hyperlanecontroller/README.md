# HyperlaneController

`outbe-hyperlanecontroller` is the governance-owned controller of the Hyperlane
bridge. It is the owner of every Hyperlane core contract on Outbe (Mailbox,
ProxyAdmin, IGP, gas oracle, ProtocolFee, InterchainAccountRouter,
StorageMessageIdMultisigIsm) and, through its Interchain Account (ICA), of the
same contracts on Ethereum / BSC. The single deployer key that owns them today
is replaced by this precompile.

Address: `0x000000000000000000000000000000000000EE14` (`HYPERLANE_CONTROLLER_ADDRESS`).

## State

| slot | field           | meaning                                                               |
|------|-----------------|-----------------------------------------------------------------------|
| 0    | `ica_router`    | InterchainAccountRouter on Outbe; zero = not initialized              |
| 1    | `ism_by_domain` | `domain -> StorageMessageIdMultisigIsm`, Outbe included under its own domain (= chain id) |
| 2    | `domains`       | enumerable list of the configured domains                             |
| 3    | `hook_by_domain` | `domain -> MerkleTreeHook`; part of the checkpoint digest validators sign |
| 4    | `signer_of`     | `validator -> Hyperlane signing key`; zero = the validator address     |
| 5    | `submitted_index` | `(validator, domain) -> latest submitted checkpoint index`          |
| 6    | `submitted_block` | `(validator, domain) -> block of that submission`; zero = never     |
| 7    | `miss_count`    | `validator -> consecutive liveness-window misses`                     |
| 8    | `validator_announce` | ValidatorAnnounce on Outbe; `outbe-feeder` reads the validator's bucket location from it |
| 9    | `bootstrap_admin` | the only caller allowed to `initialize`; genesis seeds it          |
| 10   | `approved_payload` | `proposal id -> keccak256(payload)` of an approved crosschain action; zero = none or applied |
| 11   | `approved_at`   | `proposal id -> block of the approval`                                |

Validator sets and thresholds are **not** stored here: the ISMs are the source
of truth, the controller only forwards owner calls to them.

## Direct selectors

- `initialize(icaRouter, validatorAnnounce, domains[], isms[], hooks[])` — one-shot. Caller must be the current
  owner of the Outbe ISM (the deployer that staged `transferOwnership` to the
  controller). The controller `acceptOwnership()`s the Outbe ISM (it is Ownable2Step),
  dispatches `acceptOwnership()` to every remote ISM through its ICA (so `fund()` must
  come first),
  verifies it already owns `icaRouter`, and stores the table.
- `fund()` payable — tops up the balance that pays IGP fees for ICA dispatches.
- `sync()` — permissionless. Mirrors the active Outbe validator set into every ISM:
  validators = active validators, threshold = `ceil(2n/3)` (the vote quorum rule).
  No-op when the local ISM already matches, so a keeper can call it every epoch;
  new validators become bridge signers as soon as someone calls it after their
  boundary activation. The ISM entry of a validator is its Hyperlane signer:
  the key registered with `setHyperlaneSigner`, else the validator address.
  The `BoundaryOutcome` system tx sub-calls `sync()` right after every epoch
  boundary activation (`lifecycle::sync_validators`), best effort: a failure is
  logged, never blocks the boundary.
- `submitCheckpoint(domain, root, index, messageId, signature)` — liveness proof,
  see below. Caller: an active validator or its oracle delegate (the feeder key).
- `setHyperlaneSigner(signer)` — registers the key the validator's Hyperlane agent
  signs with when it differs from the validator address; zero resets.
- `applyApproved(proposalId, payload)` — permissionless. Runs a crosschain action
  that a vote approved (see below).
- views: `icaRouter()`, `validatorAnnounce()`, `ismByDomain(domain)`, `hookByDomain(domain)`, `domains()`,
  `hyperlaneSigner(validator)`, `submittedIndex(validator, domain)`, `missCount(validator)`.

## Liveness

A validator whose Hyperlane agent stops signing stays in the ISMs and silently
lowers the bridge quorum. To catch that, `outbe-feeder` reads the validator's own
checkpoint bucket and submits the latest signed checkpoint per domain through
`submitCheckpoint`. The controller resolves the sender to its validator, recovers
the signer over the Hyperlane digest (`checkpoint_digest`: domain hash, root,
index, message id, EIP-191), requires it to match `hyperlaneSigner(validator)`
and a strictly higher index than the stored one (so index 0 is never
submitted and a repeat reverts), then records only `(index, block)`.

Every `LIVENESS_WINDOW_BLOCKS` (150) the `OracleSlashWindow` system tx runs
`lifecycle::run_liveness_window` → `check_liveness()`:

- per domain the reference index is the `threshold`-th highest submission among
  active validators, counting only submissions older than `GRACE_BLOCKS` (30);
  one validator cannot inflate it, a checkpoint everyone is still catching up on
  is ignored, an idle bridge yields no misses;
- a validator below the reference on any domain gets a miss (`LivenessMiss`),
  a validator at or above it resets its counter;
- `MAX_MISSES` (3) consecutive misses jail it without slashing (`LivenessJailed`)
  and the ISMs are re-synced in the same block;
- a validator with no submission yet is stamped and evaluated from the next window.

This catches outages, not malice: a validator can sign checkpoints only for
itself, so a fake self-signed checkpoint is indistinguishable from a real one.

## Owner operations (Rust methods)

| method                                   | effect                                                                 |
|------------------------------------------|------------------------------------------------------------------------|
| `set_validators_and_threshold(set, t)`   | full rotation: `callRemote` to every remote ISM, then the local setter; one checkpoint |
| `call_remote(domain, calls[])`           | generic ICA call on a remote chain (e.g. `acceptOwnership()` on a remote ISM) |
| `call_local(to, value, data)`            | generic owner call on Outbe (IGP, ProtocolFee, ProxyAdmin, router …)   |
| `add_domain(domain, ism, hook)` / `remove_domain(domain)` | connect / disconnect a remote chain (adding dispatches `acceptOwnership()` to the new ISM); the local domain is fixed at `initialize` |

`sync` and liveness call `set_validators_and_threshold`. The crosschain vote
target below calls the other four. No proposal can carry a free `to` and `data`.

## Crosschain vote target (OIP-00354)

`CrosschainVoteTarget` serves this address. Validators vote on one strict JSON
action. The tally runs where sub-calls are not available, so an approved
proposal only stores `keccak256(payload)` and the approval block. Anyone then
calls `applyApproved(proposalId, payload)` with the same bytes. The controller
runs the action once, as `msg.sender` on Outbe and through its Interchain
Account on a remote chain. An action older than the governance voting window
is dropped with `ApprovedActionExpired`, so a stale approval cannot undo a
newer one.

A chain id is also the Hyperlane domain of that chain. An action on the Outbe
chain id runs through `call_local` (all calls in one checkpoint). An action on
another chain id runs through one `call_remote`. A `link…` action does both:
the Outbe side first, then one message with the mirror calls for the remote
side.

The bridge is `ERC7786Bridge` in `contracts/crosschain`: one per chain, between
the token bridges and the transport adapters.

| `operation` | fields | calls |
|---|---|---|
| `setBridgePaused` | `chainId`, `bridge`, `paused` | `bridge.pause()` or `bridge.unpause()` |
| `setBridgeGateway` | `chainId`, `bridge`, `destinationChainId`, `adapter` | `bridge.setGateway(destinationChainId, adapter)`: the adapter the bridge sends through; a zero `adapter` clears the override |
| `linkBridges` | `remoteChainId`, `bridge`, `adapter`, `remoteBridge`, `remoteAdapter` (the Hyperlane adapters) | on each side: `adapter.setRouterWithChain` (the other adapter), `bridge.registerRemoteBridge` (ERC-7930 of the other bridge), `bridge.setGateway(other chain, adapter)` |
| `linkTokenBridges` | `remoteChainId`, `tokenBridge`, `remoteTokenBridge` | on each side: `tokenBridge.addPeer(other chain, other token bridge)` |
| `addHyperlaneChain` | `chainId`, `ism`, `hook` | `add_domain` |
| `removeHyperlaneChain` | `chainId` | `remove_domain` |

```json
{"operation":"setBridgePaused","chainId":42161,"bridge":"0x…","paused":true}
```

The controller does not check what the addresses are. Voters check them:
`token()` of both token bridges for `linkTokenBridges`, and the bridge and
adapter addresses on both chains for `linkBridges`. A peer link cannot be
removed.

A remote action is asynchronous: `applyApproved` succeeds when the message is
dispatched, not when the remote chain runs it. The controller pays the
Hyperlane fee from its balance.

Remote dispatch quotes `quoteGasPayment(domain)` on the router and pays it from
the controller's balance; an insufficient balance reverts before anything is sent.

## Deploy order (hyperline)

1. `mise run deploy-contracts` — stock contracts, owner = deployer key.
2. `mise run controller:init` — runs the chain `ica:enroll` (link the
   InterchainAccountRouters both ways, while the deployer still owns them) →
   `owner:transfer` (Outbe contracts → controller, remote contracts → the
   controller's ICA computed on the remote router; the ISM transfer is two-step
   and stays *pending* until accepted) → `initialize(icaRouter, validatorAnnounce,
   domains, isms, hooks)` from the deployer key (still `owner()` of the Outbe ISM).
   Each step is idempotent and can be run on its own. `fund()` the controller
   before this step: `initialize` dispatches `acceptOwnership()` to every remote
   ISM through the ICA and pays the IGP quote for each.
3. Test rotation; verify `validatorsAndThreshold` changed on the remote ISM.
