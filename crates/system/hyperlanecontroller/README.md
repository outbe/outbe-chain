# HyperlaneController

`outbe-hyperlanecontroller` keeps the Hyperlane infrastructure of the owner
address: the validator set in the ISMs, liveness, and the domain registry. The
owner address owns every Hyperlane core contract on Outbe (Mailbox, ProxyAdmin,
IGP, gas oracle, ProtocolFee, InterchainAccountRouter,
StorageMessageIdMultisigIsm) and, through its Interchain Account (ICA), the same
contracts on every remote chain.

The precompile at the address is in `crosschaingov` (OIP-00369). It dispatches
the Hyperlane selectors below to this crate, and it runs every owner call that
this crate makes (`OwnerCalls`).

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

Slots from 32 belong to `crosschaingov` at the same address.

Validator sets and thresholds are **not** stored here: the ISMs are the source
of truth, the controller only forwards owner calls to them.

## Direct selectors

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
- `fund()`, `applyApproved(proposalId, payload)` and `remoteAccount(domain)` are
  selectors of `crosschaingov`.
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

Every write to an ISM or the ICA router goes through `OwnerCalls`
(`call_local`, `call_remote`). `crosschaingov` implements it.

| method | effect |
|---|---|
| `set_validators_and_threshold(owner, set, t)` | full rotation: `call_remote` to every remote ISM, then `call_local` on the Outbe ISM, in one checkpoint. `sync` and liveness call it |
| `add_domain(owner, domain, ism, hook)` / `remove_domain(domain)` | start / stop the validator sync to a remote chain. Adding sends `acceptOwnership()` to its ISM. The local domain is fixed at `initialize` |

## Hyperlane vote actions

`actions.rs` holds the vote actions on the Hyperlane chains. The one vote target
of the address is in `crosschaingov`. It sends these operations here and the
other operations to its own actions.

| `operation` | fields | calls |
|---|---|---|
| `addHyperlaneChain` | `chainId`, `icaRouter`, `ism`, `hook` | the Outbe ICA router enrolls `icaRouter` (a no-op if the same router is enrolled, a revert if another one is), then `add_domain` |
| `removeHyperlaneChain` | `chainId` | `remove_domain` |

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

## Adding a chain after the hand-over

The deployer no longer owns the Outbe contracts, so the Outbe side of a new
chain goes through votes.

1. In `hyperline`, with the deployer key on the new chain:
   1. Add the chain to `registry/chains` and to the `NETWORK` profile.
   2. Run `core deploy`.
   3. Run `ica:enroll`. Only the new chain's side applies. The Outbe router is
      enrolled by `addHyperlaneChain`.
   4. Run `owner:transfer`. Everything goes to the controller's ICA, and the ISM
      stays pending.
   5. Configure the validator and relayer agents for the chain.
2. In `contracts`, deploy the crosschain contracts (`DeployAll`, which hands
   over to the ICA) and the token bridges on the new chain.
3. `fund()` the controller. Each remote action pays the Hyperlane fee.
4. Vote, then call `applyApproved` for each action:
   1. `addHyperlaneChain`;
   2. `linkBridges`;
   3. `linkTokenBridges` for each token.

Known gap: `call_remote` first asks the Outbe IGP for `quoteGasPayment`. The
IGP reverts for a domain without a gas oracle config, and only its owner (the
controller) can add one. No vote action sets it yet, so step 4.1 fails for a
chain that the deploy did not configure in the Outbe IGP.
