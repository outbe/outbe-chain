# HyperlaneController

`outbe-hyperlanecontroller` runs the Hyperlane infrastructure for governance:
the validator set in the ISMs, the liveness of the Hyperlane agents, and the
domain registry.

The crate is a library with no address of its own. It runs in the frame of the
CrosschainGov precompile (`outbe-crosschaingov`, `0x000000000000000000000000000000000000EE14`,
`CROSSCHAIN_GOV_ADDRESS`). That precompile owns every Hyperlane core contract on
Outbe (Mailbox, ProxyAdmin, IGP, gas oracle, ProtocolFee, InterchainAccountRouter,
StorageMessageIdMultisigIsm) and, through its Interchain Account (ICA), the same
contracts on every remote chain. The precompile sends the Hyperlane selectors to
`dispatch()` in this crate.

Every state-changing call to an ISM or to the ICA router goes through the trait
`Owner { call_local, call_remote }` (`owner.rs`). The precompile implements it,
so `msg.sender` of each call is `0x…EE14`. This crate does not depend on
`outbe-crosschaingov`.

| file | content |
|---|---|
| `owner.rs` | the trait `Owner` and `RemoteCall` |
| `actions.rs` | the vote actions `addHyperlaneChain` and `removeHyperlaneChain` |
| `domains.rs` | the ICA router, the ISM and the hook of each domain |
| `ism_sync.rs` | `sync` and `set_validators_and_threshold` |
| `liveness.rs` | `setHyperlaneSigner`, `submitCheckpoint` and the liveness verdict |
| `lifecycle.rs` | the begin-block entry points |
| `dispatch.rs` | the Hyperlane selectors |

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

The state lives in the account `0x…EE14`, slots 0-9. `outbe-crosschaingov`
keeps its own state from slot 10. Validator sets and thresholds are **not**
stored here: the ISMs are the source of truth, the controller only sends owner
calls to them.

## Hyperlane selectors

The CrosschainGov precompile sends these selectors here. `fund`,
`applyApproved` and `remoteAccount` run in `outbe-crosschaingov`.

- `initialize(icaRouter, validatorAnnounce, domains[], isms[], hooks[])` — one-shot. Caller must be the current
  owner of the Outbe ISM (the deployer that staged `transferOwnership` to the
  controller). The controller `acceptOwnership()`s the Outbe ISM (it is Ownable2Step),
  dispatches `acceptOwnership()` to every remote ISM through its ICA (so `fund()` must
  come first),
  verifies it already owns `icaRouter`, and stores the table.
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

| method | effect |
|---|---|
| `set_validators_and_threshold(set, t, owner)` | full rotation: `owner.call_remote` to every remote ISM, then `owner.call_local` to the local ISM, in one checkpoint. `sync` and liveness call it |
| `add_domain(domain, (ism, hook), owner)` / `remove_domain(domain)` | start / stop the validator sync to a remote chain. Adding sends `acceptOwnership()` to its ISM. The local domain is fixed at `initialize` |

## Vote actions

The one vote target of `0x…EE14` is `CrosschainVoteTarget` in
`outbe-crosschaingov`. It sends these two operations here. The payload is
strict JSON with an `operation` field. `HyperlaneAction::decode` checks it when
the proposal is created, and `HyperlaneAction::apply` runs it on
`applyApproved`.

| `operation` | fields | calls |
|---|---|---|
| `addHyperlaneChain` | `chainId`, `icaRouter`, `ism`, `hook` | the Outbe ICA router enrolls `icaRouter` (a no-op if the same router is enrolled, a revert if another one is), then `add_domain` |
| `removeHyperlaneChain` | `chainId` | `remove_domain` |

A chain id is also the Hyperlane domain of that chain. A new action that
changes the Hyperlane infrastructure goes in `actions.rs`. An action on our own
contracts goes in `outbe-crosschaingov`.

## Deploy order (hyperline)

1. `mise run deploy-contracts` — stock contracts, owner = deployer key.
2. `mise run controller:init` — runs the chain `ica:enroll` (link the
   InterchainAccountRouters both ways, while the deployer still owns them) →
   `owner:transfer` (Outbe contracts → `0x…EE14`, remote contracts → its ICA
   computed on the remote router; the ISM transfer is two-step
   and stays *pending* until accepted) → `initialize(icaRouter, validatorAnnounce,
   domains, isms, hooks)` from the deployer key (still `owner()` of the Outbe ISM).
   Each step is idempotent and can be run on its own. `fund()` `0x…EE14`
   before this step: `initialize` dispatches `acceptOwnership()` to every remote
   ISM through the ICA and pays the IGP quote for each.
3. Test rotation; verify `validatorsAndThreshold` changed on the remote ISM.

## Adding a chain after the hand-over

The deployer no longer owns the Outbe contracts, so the Outbe side of a new
chain goes through votes.

1. In `hyperline`, with the deployer key on the new chain:
   1. Add the chain to `registry/chains` and to the `NETWORK` profile.
   2. Run `core deploy`.
   3. Run `ica:enroll`. Only the new chain's side applies. The Outbe router is
      enrolled by `addHyperlaneChain`.
   4. Run `owner:transfer`. Everything goes to the ICA of `0x…EE14`, and the
      ISM stays pending.
   5. Configure the validator and relayer agents for the chain.
2. In `contracts`, deploy the crosschain contracts, the token bridges and the
   intent contracts on the new chain. Each `DeployAll` hands over to the ICA.
3. `fund()` `0x…EE14`. Each remote action pays the Hyperlane fee.
4. Vote, then call `applyApproved` for each action:
   1. `addHyperlaneChain`;
   2. `linkBridges`;
   3. `linkTokenBridges` for each token.

Known gap: `call_remote` first asks the Outbe IGP for `quoteGasPayment`. The
IGP reverts for a domain without a gas oracle config, and only its owner
(`0x…EE14`) can add one. No vote action sets it yet, so step 4.1 fails for a
chain that the deploy did not configure in the Outbe IGP.
