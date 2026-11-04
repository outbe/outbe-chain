# outbe-hyperlane-attester

Proves on-chain that this validator's Hyperlane agent keeps signing. It reads the
latest signed checkpoint per domain from the validator's own checkpoint bucket and
submits it to the `HyperlaneController` precompile (`0x…EE14`) with
`submitCheckpoint`. The controller jails validators that fall behind the quorum for
`MAX_MISSES` liveness windows in a row (see
[`liveness.rs`](../../crates/system/hyperlanecontroller/src/liveness.rs)).

Runs on the validator host next to `outbe-feeder`. Use a separate Hyperlane
delegate key so the services have independent transaction nonces.

## Operational keys

Use three distinct addresses for each validator:

| Address | Purpose | Binding |
|---|---|---|
| `V` | Validator, stake and TEE renewal | Existing validator identity |
| `P` | Price-feeder transactions | `V` calls `ValidatorSet.setDelegate(1, P)` |
| `H` | Hyperlane agent and attester | `V` calls `ValidatorSet.setDelegate(2, H)` |

The attester uses `H` as its transaction sender and sets `validator_address`
to `V`. Register the agent's checkpoint signing key with
`HyperlaneController.setHyperlaneSigner(H)` from the Hyperlane delegate.
That mapping selects the checkpoint signature key. Delegation authorizes
the transaction sender. These are separate checks.

Oracle delegation does not authorize Hyperlane calls. Hyperlane delegation
does not authorize Oracle votes. Rotation or revocation changes only its
assigned role. After explicit Hyperlane delegation, `V` cannot submit
checkpoints or change the checkpoint signer. `V` can rotate or revoke `H`
through ValidatorSet. Without an explicit Hyperlane delegate, the active
validator key remains accepted for Hyperlane calls.

Coordinate agent announcements and attester transactions that use `H`.

## Quick Start

```bash
cargo build -p outbe-hyperlane-attester
./target/debug/outbe-hyperlane-attester --config hyperlane-attester.toml
```

## Configuration

```toml
[chain]
rpc_endpoint = "http://127.0.0.1:8545"
chain_id = 54322345

[account]
private_key = "0x..."                                   # Hyperlane delegate key H
validator_address = "0x1111111111111111111111111111111111111111"

[hyperlane]
poll_interval_secs = 30   # optional, default 30
gasless = true            # optional, default false: ZeroFee txpool policy

[health]
enabled = true            # optional, default true
bind_address = "0.0.0.0:9003"
```

Nothing about the bucket is configured: every poll the attester reads
`HyperlaneController.domains()` and `validatorAnnounce()`, then the validator's
latest announced location from `ValidatorAnnounce.getAnnouncedStorageLocations`
(`s3+http://host:port/<validator>/<folder>`), and fetches
`<bucket>/<domain>/checkpoint_latest_index.json` plus
`checkpoint_<index>_with_id.json`. The Hyperlane validator must therefore run with
`--checkpointSyncer.folder=<domain id>`. A host move needs no attester change: the
agent re-announces, the attester follows, like the relayer.

A checkpoint is submitted only when its index is higher than
`submittedIndex(validator, domain)`; index 0 is never submitted. Until the
controller is initialized (`domains()` empty) the attester idles.

`gasless = true` sends `submitCheckpoint` with zero priority fee; the node's
`HyperlaneSubmitCheckpointHook` waives the fee for active validators and their
Hyperlane delegates. An explicit Hyperlane delegate replaces the validator
key for this authorization.

## Health

`GET /health` returns 200 while `missCount(validator)` on the controller is zero
and 503 otherwise: a non-zero value means the next misses lead to a jail, so alert
on it. `GET /status` returns `submitted`, `failed`, `last_submit_time` and
`miss_count`.
