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
| 0    | `router`        | InterchainAccountRouter on Outbe; zero = not initialized              |
| 1    | `ism_by_domain` | `domain -> StorageMessageIdMultisigIsm`, Outbe included under its own domain (= chain id) |
| 2    | `domains`       | enumerable list of the configured domains                             |
| 3    | `hook_by_domain` | `domain -> MerkleTreeHook`; part of the checkpoint digest validators sign |
| 4    | `signer_of`     | `validator -> Hyperlane signing key`; zero = the validator address     |
| 5    | `submitted_index` | `(validator, domain) -> latest submitted checkpoint index`          |
| 6    | `submitted_block` | `(validator, domain) -> block of that submission`; zero = never     |
| 7    | `miss_count`    | `validator -> consecutive liveness-window misses`                     |

Validator sets and thresholds are **not** stored here: the ISMs are the source
of truth, the controller only forwards owner calls to them.

## Direct selectors

- `initialize(router, domains[], isms[], hooks[])` — one-shot. Caller must be the current
  owner of the Outbe ISM (the deployer that staged `transferOwnership` to the
  controller). The controller `acceptOwnership()`s the ISM (it is Ownable2Step),
  verifies it already owns `router`, and stores the table.
- `fund()` payable — tops up the balance that pays IGP fees for ICA dispatches.
- `sync()` — permissionless. Mirrors the active Outbe validator set into every ISM:
  validators = active validators, threshold = `ceil(2n/3)` (the vote quorum rule).
  No-op when the local ISM already matches, so a keeper can call it every epoch;
  new validators become bridge signers as soon as someone calls it after their
  boundary activation. Hyperlane validator keys are the validators' own addresses.
  The `BoundaryOutcome` system tx sub-calls `sync()` right after every epoch
  boundary activation (`lifecycle::sync_validators`), best effort: a failure is
  logged, never blocks the boundary.
- `submitCheckpoint(domain, root, index, messageId, signature)` — liveness proof,
  see below. Caller: an active validator or its oracle delegate (the feeder key).
- `setHyperlaneSigner(signer)` — registers the key the validator's Hyperlane agent
  signs with when it differs from the validator address; zero resets.
- views: `router()`, `ismByDomain(domain)`, `hookByDomain(domain)`, `domains()`,
  `hyperlaneSigner(validator)`, `submittedIndex(validator, domain)`, `missCount(validator)`.

## Liveness

A validator whose Hyperlane agent stops signing stays in the ISMs and silently
lowers the bridge quorum. To catch that, `outbe-feeder` reads the validator's own
checkpoint bucket and submits the latest signed checkpoint per domain through
`submitCheckpoint`. The controller resolves the sender to its validator, recovers
the signer over the Hyperlane digest (`checkpoint_digest`: domain hash, root,
index, message id, EIP-191), requires it to match `hyperlaneSigner(validator)`
and a non-decreasing index, then records only `(index, block)`.

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

## Owner operations (Rust methods, trigger not wired yet)

| method                                   | effect                                                                 |
|------------------------------------------|------------------------------------------------------------------------|
| `add_validator(v, threshold?)`           | reads the current set from the Outbe ISM, appends, pushes to all ISMs  |
| `remove_validator(v, threshold?)`        | same, removing                                                         |
| `set_threshold(t)`                       | same set, new threshold                                                |
| `set_validators_and_threshold(set, t)`   | full rotation: `callRemote` to every remote ISM, then the local setter; one checkpoint |
| `call_remote(domain, calls[])`           | generic ICA call on a remote chain (e.g. `acceptOwnership()` on a remote ISM) |
| `call_local(to, value, data)`            | generic owner call on Outbe (IGP, ProtocolFee, ProxyAdmin, router …)   |
| `add_domain(domain, ism, hook)` / `remove_domain(domain)` | connect / disconnect a remote chain; the local domain is fixed at `initialize` |

Remote dispatch quotes `quoteGasPayment(domain)` on the router and pays it from
the controller's balance; an insufficient balance reverts before anything is sent.

## Deploy order (hyperline)

1. `mise run deploy-contracts` — stock contracts, owner = deployer key.
2. `mise run ica:enroll` — link the InterchainAccountRouters both ways.
3. `mise run owner:transfer` — Outbe contracts → controller, remote contracts →
   the controller's ICA (computed on the remote router). The ISM transfer is
   two-step: it stays *pending* until accepted.
4. `initialize(router, domains, isms, hooks)` from the deployer key (still `owner()` of the Outbe ISM).
5. `fund()`.
6. `call_remote(domain, [ism.acceptOwnership()])` per remote chain.
7. Test rotation; verify `validatorsAndThreshold` changed on the remote ISM.
