# outbe-hyperlane-validator

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
| `H` | Hyperlane agent and `outbe-hyperlane-validator` | `V` calls `ValidatorSet.setDelegate(2, H)` |

`outbe-hyperlane-validator` uses `H` as its transaction sender and sets `validator_address`
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

Coordinate agent announcements and `outbe-hyperlane-validator` transactions that use `H`.

## Quick Start

```bash
cargo build -p outbe-hyperlane-validator
./target/debug/outbe-hyperlane-validator --config hyperlane-validator.toml
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

[storage]                 # optional, only the db commands use it now
db_path = "/var/lib/outbe-hyperlane-validator/db"
```

The configuration has no bucket settings. On every poll, the daemon:

1. Reads `HyperlaneController.domains()` and
   `validatorAnnounceByDomain(<Outbe chain id>)`.
2. Reads the latest announced location of the validator from
   `ValidatorAnnounce.getAnnouncedStorageLocations`
   (`s3+http://host:port/<validator>/<folder>`).
3. Fetches `<bucket>/<domain>/checkpoint_latest_index.json` and
   `checkpoint_<index>_with_id.json`.

The Hyperlane agent must run with `--checkpointSyncer.folder=<domain id>`.
A host move needs no change in this daemon. The agent announces the new
location, and the daemon reads it on the next poll, as the relayer does.

The daemon submits a checkpoint only when its index is higher than
`submittedIndex(validator, domain)`. It never submits index 0. Until the
controller is initialized (`domains()` is empty), the daemon does nothing.

`gasless = true` sends `submitCheckpoint` with zero priority fee; the node's
`HyperlaneSubmitCheckpointHook` waives the fee for active validators and their
Hyperlane delegates. An explicit Hyperlane delegate replaces the validator
key for this authorization.

## Origin chains

Each `[[origin]]` block configures one origin chain. The daemon does not use
these blocks yet. A later step of the validator port starts the sync.

```toml
[[origin]]
domain = 54322345
rpc_urls = ["https://rpc.testnet.outbe.net"]
# Number of rpc_urls that must return the same root before the validator signs:
#   "majority" - N - floor(N/3), at least 2/3 (default)
#   "quorum"   - ceil(N/2), at least 1/2
#   "single"   - the first RPC that answers, no vote
rpc_consensus = "majority"
reorg_period = 0                          # Blocks, or "finalized".
interval_secs = 15                        # Poll interval without WebSocket, or the fallback poll with it.
# ws_url = "wss://…"                      # Optional. Enable after the Outbe RPC nodes serve WebSocket.
# index_from = 4782                       # Optional. Default: deployedBlock() of the hook.

[[origin]]
domain = 97
rpc_urls = ["https://data-seed-prebsc-1-s1.bnbchain.org:8545", "https://data-seed-prebsc-1-s2.bnbchain.org:8545", "https://data-seed-prebsc-2-s1.bnbchain.org:8545"]
rpc_consensus = "majority"                # majority (default) | quorum | single
reorg_period = 9
interval_secs = 15
ws_url = "wss://…"
```

The configuration has no contract addresses. The daemon reads them from the
chains:

| Value | Source |
|---|---|
| Domains | `HyperlaneController.domains()` |
| MerkleTreeHook | `HyperlaneController.hookByDomain(domain)` |
| ValidatorAnnounce | `HyperlaneController.validatorAnnounceByDomain(domain)` |
| Mailbox | `ValidatorAnnounce.mailbox()` on the origin chain |
| First block | `index_from`, else `MerkleTreeHook.deployedBlock()` |

While `icaRouter()` is zero, the controller is not initialized and the daemon
waits. Before it uses a domain, the daemon checks these conditions:

1. `ValidatorAnnounce.localDomain()` is equal to the domain.
2. `MerkleTreeHook.mailbox()` is equal to `ValidatorAnnounce.mailbox()`.

A domain of the controller without an `[[origin]]` block is an error of that
domain only. The other domains continue.

The sync reads `InsertedIntoTree` logs only up to the finalized block, in
ranges of at most 100,000 blocks. The Outbe RPC rejects larger ranges. A new
leaf becomes verified only when enough `rpc_urls` return the same
`latestCheckpoint()` as the local tree. One `rpc_urls` entry has one vote.
The daemon removes duplicate URLs. A WebSocket event only starts the next
poll earlier. The event does not verify a leaf.

## Local store

`[storage] db_path` sets the directory of the validator's own MDBX store.
Do not use the node database directory. The daemon does not use the store
yet. Later steps of the validator port write leaves and signed checkpoints
to it.

The store opens in MDBX exclusive mode. While one process holds the store,
a second process cannot open it. Stop the daemon before you run a `db`
command.

| Command | What it does |
|---|---|
| `db status` | For each domain: leaf count, latest signed index, cursor, reorg flag. |
| `db checkpoint --domain D --index I` | Prints the stored leaf and the signed root at index `I`. |
| `db clear-reorg --domain D` | Removes the reorg flag of domain `D`. Do this only after you fix the cause. |

## Signing

The daemon does not run the signing yet. A later step of the validator port
starts it. Each origin domain has one signing loop and one backfill worker.
The signer key is `account.private_key` (`H`).

1. The signing loop gets the verified leaves of a sync poll. In one store
   transaction, it writes all the leaves, the cursor, and the signed
   checkpoint of the last leaf. Relayers and the liveness check get the
   newest index at once.
2. The backfill worker then signs each index that has a leaf but no
   checkpoint, in index order. It starts at the first unsigned index.
   On the first start, it signs from index 0. It writes one transaction
   for each 1,000 leaves.

The daemon never signs a second root for an index that it signed before.

### Reorg flag

The daemon halts a domain when one of these conditions occurs:

- Enough `rpc_urls` agree on a different root at a signed index.
- A stored signed root differs from the root of the stored leaves.

The daemon logs the roots of each endpoint and the local root. Then it
writes the reorg flag of the domain to the store. It retries until the
write succeeds. The signing loop and the backfill worker of that domain
stop. The other domains continue, and the process does not stop.

At start, a domain with the reorg flag does not sync or sign. To clear
the flag, find and fix the cause. Then stop the daemon and run
`db clear-reorg --domain D`.

## Health

`GET /health` returns 200 while `missCount(validator)` on the controller is zero
and 503 otherwise: a non-zero value means the next misses lead to a jail, so alert
on it. `GET /status` returns `submitted`, `failed`, `last_submit_time` and
`miss_count`.
