# outbe-hyperlane-validator

## What it is

`outbe-hyperlane-validator` is the Hyperlane validator of an Outbe validator,
in one process. It replaces three services: the Docker Hyperlane validator
agent, MinIO and the attester. The sync, signing, announce and checkpoint
formats are ported from hyperlane-monorepo `agents-v3.0.0` (Apache-2.0, see
[`LICENSE-APACHE`](LICENSE-APACHE)). The ported files name their upstream
source and the changes.

## How it works

```text
           HyperlaneController (0x…EE14)
                 │ domains, hooks, ValidatorAnnounce
                 ▼
  origin RPCs ──► sync + RPC vote ──► sign ──► MDBX store ──► :9000 S3 GET ──► relayer
                                                   │
                                                   ├──► announce (each origin chain)
                                                   └──► submitCheckpoint (Outbe)
```

1. **Discovery.** Every `poll_interval_secs`, the daemon reads the domains
   and their contracts from the `HyperlaneController` precompile.
2. **Sync.** For each domain, the daemon reads `InsertedIntoTree` logs of the
   `MerkleTreeHook` up to the finalized block. A leaf becomes verified only
   when enough `rpc_urls` return the same `latestCheckpoint()` as the local
   tree (the RPC vote).
3. **Sign.** The daemon signs the latest verified leaf first. A backfill
   worker then signs every earlier leaf in index order.
4. **Store.** Leaves, signed roots and the sync cursor go to a local MDBX
   store.
5. **Serve.** Port 9000 serves the signed checkpoints as an S3-compatible
   bucket. The relayer reads them with its S3 client.
6. **Announce.** The daemon announces the bucket on the `ValidatorAnnounce`
   of each origin chain, once.
7. **Liveness.** The daemon sends the latest signed checkpoint of each domain
   to the controller with `submitCheckpoint`. The controller jails validators
   that fall behind the quorum for `MAX_MISSES` liveness windows in a row (see
   [`liveness.rs`](../../crates/system/hyperlanecontroller/src/liveness.rs)).

## Keys

| Address | Purpose | Binding |
|---|---|---|
| `V` | Validator, stake and TEE renewal | Existing validator identity. Set it as `validator_address`. |
| `H` | Checkpoint signer, announce sender, `submitCheckpoint` sender | `V` calls `ValidatorSet.setDelegate(2, H)`. Set it as `private_key`. |

The price feeder uses its own key `P` (`ValidatorSet.setDelegate(1, P)`).
Use three distinct addresses.

Register `H` as the checkpoint signer with
`HyperlaneController.setHyperlaneSigner(H)` from the Hyperlane delegate.
That mapping selects the checkpoint signature key. Delegation authorizes the
transaction sender. These are separate checks. Each liveness tick reads
`hyperlaneSigner(V)` (`V` when it is zero). When it is not `H`, the daemon
does not submit checkpoints and `/status` shows `signer_error`.

Oracle delegation does not authorize Hyperlane calls. Hyperlane delegation
does not authorize Oracle votes. Rotation or revocation changes only its
assigned role. After explicit Hyperlane delegation, `V` cannot submit
checkpoints or change the checkpoint signer. `V` can rotate or revoke `H`
through ValidatorSet. Without an explicit Hyperlane delegate, the active
validator key remains accepted for Hyperlane calls.

| Rule | Detail |
|---|---|
| Bucket name | The lowercase `0x` address of `H`. |
| Gas for announce | `H` needs native gas on every origin chain, Outbe included. The announce is a normal transaction. |
| Gas for liveness | With `gasless = true`, `submitCheckpoint` on Outbe uses the ZeroFee policy and costs no gas. |
| One process per `H` | Do not run a second process with `H`. Both processes would use the same nonces. |

## Quick start

```bash
cargo build --release -p outbe-hyperlane-validator
cp bin/outbe-hyperlane-validator/hyperlane-validator.example.toml hyperlane-validator.toml
# Set private_key, validator_address, public_url and db_path.
./target/release/outbe-hyperlane-validator --config hyperlane-validator.toml
```

Without `--config`, the binary reads `hyperlane-validator.toml` in the
current directory. Set the log level with `RUST_LOG` (default
`outbe_hyperlane_validator=info`).

## Configuration

[`hyperlane-validator.example.toml`](hyperlane-validator.example.toml) is a
complete testnet example. A unit test parses and validates it, so it always
matches the code. Unknown fields are errors.

| Field | Default | Meaning |
|---|---|---|
| `[chain] rpc_endpoint` | required | Outbe JSON-RPC (HTTP). Discovery and liveness use it. |
| `[chain] chain_id` | required | Outbe chain id for `submitCheckpoint` transactions. |
| `[account] private_key` | required | Hex key of `H`. It is plaintext: protect the file. |
| `[account] validator_address` | required | Address of `V`. |
| `[hyperlane] poll_interval_secs` | `30` | Interval of the controller poll and of the liveness loop. Must be > 0. |
| `[hyperlane] gasless` | `false` | Send `submitCheckpoint` with zero priority fee (ZeroFee txpool policy). |
| `[health] enabled` | `true` | Start the health server. Without a `[health]` section, it starts. |
| `[health] bind_address` | `0.0.0.0:9003` | Address of `/health` and `/status`. |
| `[storage] db_path` | required | Directory of the MDBX store. Never the node database. |
| `[server] bind_address` | `0.0.0.0:9000` | Address of the S3-compatible read endpoint. |
| `[server] public_url` | required | `http(s)://host[:port]` that relayers use. No path. The announced location comes from it. |
| `[[origin]] domain` | required | Hyperlane domain of the origin chain. One block per domain. |
| `[[origin]] rpc_urls` | required | HTTP endpoints. Each unique URL has one vote. Duplicates are removed. |
| `[[origin]] rpc_consensus` | `"majority"` | Votes for a root: `"majority"` = N − floor(N/3), `"quorum"` = ceil(N/2), `"single"` = 1. |
| `[[origin]] reorg_period` | required | Blocks behind the head (an integer), or `"finalized"`. |
| `[[origin]] interval_secs` | `15` | Sync poll interval. With `ws_url`, the fallback poll. Must be > 0. |
| `[[origin]] ws_url` | none | WebSocket endpoint. Its events only start the next poll earlier. They verify nothing. |
| `[[origin]] index_from` | `deployedBlock()` of the hook | First origin block to read. |

The configuration has no contract addresses. The daemon reads them from the
chains:

| Value | Source |
|---|---|
| Domains | `HyperlaneController.domains()` |
| MerkleTreeHook | `HyperlaneController.hookByDomain(domain)` |
| ValidatorAnnounce | `HyperlaneController.validatorAnnounceByDomain(domain)` |
| Mailbox | `ValidatorAnnounce.mailbox()` on the origin chain |
| First block | `index_from`, else `MerkleTreeHook.deployedBlock()` |
| Active set | `ValidatorSet.getActiveValidators()` (`0x…EE00`) |

Before it starts a domain, the daemon checks these conditions on the origin
chain:

1. `ValidatorAnnounce.localDomain()` is equal to the domain.
2. `MerkleTreeHook.mailbox()` is equal to `ValidatorAnnounce.mailbox()`.

Domain lifecycle:

| Event | Behavior |
|---|---|
| `icaRouter()` is zero | The controller is not initialized. The daemon waits and polls again. It does not stop. |
| New domain with an `[[origin]]` block | The domain starts: sync, signing, backfill, optional WebSocket waker, announce. |
| Domain without an `[[origin]]` block | An error of that domain only. The other domains continue. |
| `hookByDomain` changes | The daemon stops that domain and shows an error. The domain does not start again in this process. |
| Controller removes the domain | The daemon stops the domain. |

## Ports

| Port | Who reads it | Endpoints |
|---|---|---|
| 9000 | The relayer | S3-compatible anonymous `GET` and `HEAD`. Allow only the relayer address in the firewall. |
| 9003 | Monitoring | `GET /health`, `GET /status` |

Port 9000 uses path-style requests: `GET /<bucket>/<domain>/<key>`.

| Key | Body |
|---|---|
| `checkpoint_latest_index.json` | The latest signed index, a JSON number. |
| `checkpoint_<index>_with_id.json` | The signed checkpoint, the same bytes as the upstream agent. |
| `announcement.json` | The signed announcement, pretty JSON. |

A missing key returns 404 with the S3 `NoSuchKey` XML. Another bucket returns
404 `NoSuchBucket`. The server reads only the store, so it serves stored
checkpoints also while the Outbe RPC is down. A request path never maps to a
file.

`GET /health` returns 503 when one of these conditions is true, and 200
otherwise:

- `missCount(V)` is not zero. The next misses lead to a jail, so alert on it.
- `H` is not `hyperlaneSigner(V)`.
- A domain halted after a reorg.
- A domain has an error. A sync that makes no progress after many rewinds is
  also an error.

While the daemon waits for the controller, `/health` returns 200.

`GET /status` fields:

| Field | Meaning |
|---|---|
| `healthy` | The `/health` result. |
| `controller` | `unknown`, `waiting_for_controller` or `ready`. |
| `active` | `V` is in the active validator set. `null` before the first read. |
| `signer_error` | Why `H` is not the accepted checkpoint signer, or `null`. |
| `miss_count` | `missCount(V)` on the controller. |
| `submitted`, `failed` | `submitCheckpoint` counters since the start. `submitted` counts only transactions with a successful receipt. A revert or no receipt is a failure. |
| `last_submit_time` | Unix time of the last successful `submitCheckpoint` receipt. `0` before the first one. |
| `last_submit_error` | The error of the last failed `submitCheckpoint`, or `null` after a success. |
| `domains.<id>.leaves` | Stored leaf count. |
| `domains.<id>.latest_signed_index` | Highest signed index. |
| `domains.<id>.cursor` | Last origin block that the store covers. |
| `domains.<id>.reorg` | The domain halted after a reorg. |
| `domains.<id>.error` | Domain error, or `null`. |

## Storage

`[storage] db_path` is the directory of the validator's own MDBX store. The
store opens in exclusive mode. While the daemon runs, no other process can
open it.

| Table | Key | Value |
|---|---|---|
| `Leaves` | (domain, index) | Message id, origin block |
| `Checkpoints` | (domain, index) | Root, 65-byte signature |
| `Cursor` | domain | Last processed finalized origin block |
| `Meta` | (domain, name) | `Announcement`, `Signer`, `ReorgFlag`, `Hook` |

The `db` commands work only while the daemon is stopped:

| Command | What it does |
|---|---|
| `db status` | For each domain: leaf count, latest signed index, cursor, reorg flag. |
| `db checkpoint --domain D --index I` | Prints the stored leaf and the signed root at index `I`. |
| `db clear-reorg --domain D` | Removes the reorg flag of domain `D`. |

## Safety

| Rule | Detail |
|---|---|
| Finality | The sync reads logs and roots only up to `latest - reorg_period`, or the `finalized` block. |
| RPC vote | A leaf is verified only when `rpc_consensus` endpoints return the same root as the local tree. A failed endpoint keeps its slot, so it never lowers the threshold. The log source alone never authorizes a root. |
| One signature per index | The daemon never signs a second root for an index that it signed before. The store rejects such a write. |
| Reorg halt | A conflict halts only its domain. The other domains and the HTTP servers continue. |
| Lost history | When `submittedIndex(V, domain)` is above the local latest index, the daemon logs a warning and continues. |

The daemon halts a domain when one of these conditions is true:

- Enough `rpc_urls` agree on a different root at a signed index.
- Enough `rpc_urls` give logs with another message id than the stored leaf at
  the same index. The count is the same as for the root vote. When fewer
  endpoints do this, the daemon reads from the next endpoint and does not halt.
- A stored signed root differs from the root of the stored leaves.

The daemon logs the root of each endpoint and the local root. Then it writes
the reorg flag and retries until the write is durable. Do not restart the
daemon while it logs `reorg flag write failed`. A domain with the flag does
not sync or sign after a restart.

Reorg recovery:

1. Read the logs. Find the cause, for example a bad RPC or a deep reorg on the
   origin chain.
2. Fix the cause, for example replace the RPC or increase `reorg_period`.
3. Stop the daemon.
4. Run `db clear-reorg --domain D`.
5. Start the daemon.

If the endpoints still agree on another root, the next vote halts the domain
again. Do not delete the store to clear a real reorg: a new sync signs a
second root for indexes that relayers already read.

Hook mismatch recovery: the store keeps the hook of each domain. If
`hookByDomain(domain)` differs from the stored hook at start, the domain does
not start and `/status` shows an error. Stop the daemon and delete the
`db_path` directory. Every domain then syncs and signs again. RFC 6979
signatures are deterministic, so the same roots get the same signatures.

Key rotation: after a change of `H`, the backfill starts at index 0 once. It
signs every stored checkpoint again with the new key. Only the signature
changes. A different root is a conflict and halts the domain. A new `H` also
gives a new bucket name, so the daemon announces again on every origin chain.

## Inactive validator

When `V` is not in `ValidatorSet.getActiveValidators()`, for example while it
is jailed:

| Part | Behavior |
|---|---|
| Sync, sign, backfill | Continue. |
| Port 9000, announce | Continue. |
| `submitCheckpoint` | Skipped. The controller would reject it. |
| `/status` | `"active": false`. This is not an error. |

While `V` is active, the daemon submits the latest signed checkpoint of each
running domain when its index is above `submittedIndex(V, domain)`. It never
submits index 0. It waits up to 60 seconds for the receipt.

## Limits

| Limit | Detail |
|---|---|
| First sync memory | Unverified leaves stay in memory until the vote verifies them. Expect about 1 GB per 5M leaves on the first sync. |
| Outbe `eth_getLogs` | The Outbe RPC rejects ranges above 100,000 blocks. The sync uses at most 100,000 and halves the range after a range error. |
| `metadata_latest.json` | Not served. The relayer does not read it. |
| Private key | The key is plaintext in the config file. |
