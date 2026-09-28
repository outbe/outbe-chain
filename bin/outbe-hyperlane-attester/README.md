# outbe-hyperlane-attester

Proves on-chain that this validator's Hyperlane agent keeps signing. It reads the
latest signed checkpoint per domain from the validator's own checkpoint bucket and
submits it to the `HyperlaneController` precompile (`0x…EE14`) with
`submitCheckpoint`. The controller jails validators that fall behind the quorum for
`MAX_MISSES` liveness windows in a row (see `crates/system/hyperlanecontroller/README.md`).

Runs on the validator host next to `outbe-feeder` and uses the same key: the
validator's oracle delegate (or the validator key itself).

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
private_key = "0x..."                                   # oracle delegate key
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
oracle delegates.

## Health

`GET /health` returns 200 while `missCount(validator)` on the controller is zero
and 503 otherwise: a non-zero value means the next misses lead to a jail, so alert
on it. `GET /status` returns `submitted`, `failed`, `last_submit_time` and
`miss_count`.
