# outbe-hyperlane-validator

The Hyperlane validator of an Outbe validator, in one process. It replaces the
upstream Hyperlane validator agent. The daemon:

1. Reads the domains and their contracts from the `HyperlaneController`
   precompile (`0x…EE14`).
2. Syncs the `MerkleTreeHook` of each origin chain and signs checkpoints into
   a local MDBX store.
3. Serves the signed checkpoints as an S3-compatible bucket on port 9000. The
   relayer reads them with its S3 client.
4. Announces the bucket on the `ValidatorAnnounce` of each origin chain.
5. Submits the latest checkpoint of each domain to the controller with
   `submitCheckpoint`. The controller jails validators that fall behind the
   quorum for `MAX_MISSES` liveness windows in a row (see
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

`outbe-hyperlane-validator` uses `H` as its checkpoint signer and its transaction
sender, and sets `validator_address` to `V`. Register `H` as the checkpoint
signer with `HyperlaneController.setHyperlaneSigner(H)` from the Hyperlane delegate.
That mapping selects the checkpoint signature key. Delegation authorizes
the transaction sender. These are separate checks.

Oracle delegation does not authorize Hyperlane calls. Hyperlane delegation
does not authorize Oracle votes. Rotation or revocation changes only its
assigned role. After explicit Hyperlane delegation, `V` cannot submit
checkpoints or change the checkpoint signer. `V` can rotate or revoke `H`
through ValidatorSet. Without an explicit Hyperlane delegate, the active
validator key remains accepted for Hyperlane calls.

Do not run a second process with `H`: both processes would use the same nonces.

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

[storage]                 # required
db_path = "/var/lib/outbe-hyperlane-validator/db"

[server]                  # required
bind_address = "0.0.0.0:9000"            # optional, default 0.0.0.0:9000
public_url = "http://203.0.113.7:9000"   # the address that relayers use
```

## Daemon loop

Every `poll_interval_secs`, the daemon reads the controller:

- While `icaRouter()` is zero, the controller is not initialized. The daemon
  waits and polls again. It does not stop.
- A new domain with an `[[origin]]` block starts: one signing loop, one
  backfill worker, an optional WebSocket waker and the announce loop.
- When `hookByDomain(domain)` changes for a running domain, the daemon stops
  that domain and shows an error in `/status`. It does not start the domain
  again in this process.
- When the controller removes a domain, the daemon stops it.

At the start of a domain, the daemon compares `submittedIndex(V, domain)` with
the local store. A higher index on-chain means that the local history was
lost. The daemon logs a warning and continues.

### Liveness

Every poll, the daemon reads `missCount(V)` and
`ValidatorSet.getActiveValidators()` (`0x…EE00`). While `V` is not active (for
example jailed), the daemon keeps signing, serving and announcing, but it does
not send `submitCheckpoint`: the controller would reject it. `/status` then
shows `"active": false`. This is not an error.

While `V` is active, for each running domain, the daemon submits the latest
signed checkpoint when its index is higher than `submittedIndex(V, domain)`.
It never submits index 0.

`gasless = true` sends `submitCheckpoint` with zero priority fee; the node's
`HyperlaneSubmitCheckpointHook` waives the fee for active validators and their
Hyperlane delegates. An explicit Hyperlane delegate replaces the validator
key for this authorization.

### Announce

For each origin chain, the daemon signs an announcement with `H` for the
location `s3+http://<public_url host:port>/<bucket>/<domain>` (`s3+https` for
an `https` URL). The bucket is the lowercase address of `H`. If
`ValidatorAnnounce.getAnnouncedStorageLocations([H])` does not contain this
location, the daemon sends `announce` on that origin chain from `H`, through
the first `rpc_urls` entry that answers. It retries with a backoff from 30 s
to 15 min. `H` must have native gas on every origin chain. On Outbe, the
announce uses the `gasless` setting.

## Checkpoint server

The server on `[server] bind_address` is an S3-compatible anonymous read,
path-style: `GET /<bucket>/<domain>/<key>`. `HEAD` also works.

| Key | Body |
|---|---|
| `checkpoint_latest_index.json` | The latest signed index, a JSON number. |
| `checkpoint_<index>_with_id.json` | The signed checkpoint, the same bytes as the upstream agent. |
| `announcement.json` | The signed announcement, pretty JSON. |

A missing key returns 404 with the S3 `NoSuchKey` XML. Another bucket returns
404 `NoSuchBucket`. The server does not have `metadata_latest.json`: the
relayer does not read it. A request path never maps to a file.

## Origin chains

Each `[[origin]]` block configures one origin chain.

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
Do not use the node database directory.

The store opens in MDBX exclusive mode. While one process holds the store,
a second process cannot open it. Stop the daemon before you run a `db`
command.

| Command | What it does |
|---|---|
| `db status` | For each domain: leaf count, latest signed index, cursor, reorg flag. |
| `db checkpoint --domain D --index I` | Prints the stored leaf and the signed root at index `I`. |
| `db clear-reorg --domain D` | Removes the reorg flag of domain `D`. Do this only after you fix the cause. |

## Signing

Each origin domain has one signing loop and one backfill worker. The signer
key is `account.private_key` (`H`).

1. The signing loop gets the verified leaves of a sync poll. In one store
   transaction, it writes all the leaves, the cursor, and the signed
   checkpoint of the last leaf. Relayers and the liveness check get the
   newest index at once.
2. The backfill worker then signs each index that has a leaf but no
   checkpoint, in index order. It starts at the first unsigned index.
   On the first start, it signs from index 0. It writes one transaction
   for each 1,000 leaves.

The daemon never signs a second root for an index that it signed before.

After a change of `H`, the backfill starts at index 0 once. It signs again
each stored checkpoint of the old key. Only the signature changes: a
different root is a conflict and halts the domain.

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

The health server listens on `[health] bind_address` (default port 9003).

`GET /health` returns 503 when one of these conditions occurs, and 200
otherwise:

- `missCount(V)` is not zero. The next misses lead to a jail, so alert on it.
- A domain halted after a reorg.
- A domain has an error (for example no `[[origin]]` block, or a changed hook).

While the daemon waits for the controller, `/health` returns 200.

`GET /status` returns:

| Field | Meaning |
|---|---|
| `healthy` | The `/health` result. |
| `controller` | `unknown`, `waiting_for_controller` or `ready`. |
| `active` | `V` is in the active validator set. `null` before the first read. |
| `miss_count` | `missCount(V)` on the controller. |
| `submitted`, `failed`, `last_submit_time` | `submitCheckpoint` counters. |
| `domains.<id>` | `leaves`, `latest_signed_index`, `cursor`, `reorg`, `error`. |
