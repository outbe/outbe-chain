# outbe-hyperlane-agent

## What it is

`outbe-hyperlane-agent` is the Hyperlane agent of an Outbe validator, in one
process. It has two roles:

- **Validator.** It signs the checkpoints of each origin chain. This role
  replaces three services: the Docker Hyperlane validator agent, MinIO and
  the attester.
- **Relayer.** It delivers the messages to their
  destination chains. This role replaces the Docker Hyperlane relayer. Every
  validator runs it. See [Relayer](#relayer).

The sync, signing, announce, checkpoint formats and the delivery flow are
ported from hyperlane-monorepo `agents-v3.0.0` (Apache-2.0, see
[`LICENSE-APACHE`](LICENSE-APACHE)). The ported files name their upstream
source and the changes.

## How it works

```text
           HyperlaneController (0x…EE14)
                 │ domains, hooks, ValidatorAnnounce
                 ▼
  origin RPCs ──► sync + RPC vote ──► sign ──► MDBX store ──► :9000 S3 GET ──► relayers
                                                   │
                                                   ├──► announce (each origin chain)
                                                   ├──► submitReport (Outbe)
                                                   └──► relayer role ──► process (destination chains)
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
7. **Liveness.** The daemon sends its root and its report of each domain to
   the controller with `submitReport`. The controller jails validators
   that fall behind the quorum for `MAX_MISSES` liveness rounds in a row (see
   [`liveness.rs`](../../crates/system/hyperlanecontroller/src/liveness.rs)).
8. **Relay.** The daemon delivers each message to its
   destination chain. See [Relayer](#relayer).

## Keys

| Address | Purpose | Binding |
|---|---|---|
| `V` | Validator, stake and TEE renewal | Existing validator identity. Set it as `validator_address`. |
| `H` | Checkpoint signer, announce sender, `submitReport` sender, relayer `process` sender | `V` calls `ValidatorSet.setDelegate(2, H)`. Set it as `private_key`. |

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
| Gas for the relayer | `H` pays for each `process` transaction on the destination chain. |
| Gas for liveness | With `gasless = true`, `submitReport` on Outbe uses the ZeroFee policy and costs no gas. |
| One process per `H` | Do not run a second process with `H`. Both processes would use the same nonces. |

## Quick start

```bash
cargo build --release -p outbe-hyperlane-agent
cp bin/outbe-hyperlane-agent/hyperlane-agent.example.toml hyperlane-agent.toml
# Set private_key, validator_address, public_url and db_path.
./target/release/outbe-hyperlane-agent --config hyperlane-agent.toml
```

Without `--config`, the binary reads `hyperlane-agent.toml` in the
current directory. Set the log level with `RUST_LOG` (default
`outbe_hyperlane_agent=info`).

## Configuration

[`hyperlane-agent.example.toml`](hyperlane-agent.example.toml) is a
complete testnet example. A unit test parses and validates it, so it always
matches the code. Unknown fields are errors.

| Field | Default | Meaning |
|---|---|---|
| `[chain] rpc_endpoint` | required | Outbe JSON-RPC (HTTP). Discovery and liveness use it. |
| `[chain] chain_id` | required | Outbe chain id for `submitReport` transactions. |
| `[account] private_key` | required | Hex key of `H`. It is plaintext: protect the file. |
| `[account] validator_address` | required | Address of `V`. |
| `[hyperlane] poll_interval_secs` | `30` | Interval of the controller poll and of the liveness loop. Must be > 0. |
| `[hyperlane] gasless` | `false` | Send `submitReport` with zero priority fee (ZeroFee txpool policy). |
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
| Multisig ISM (relayer) | `HyperlaneController.ismByDomain(domain)` |
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
| `ismByDomain` changes | The relayer takes the controller ISM of a domain as fixed. On a change it stops the deliveries of that domain and shows `relay.error`. The validator role of the domain continues. Restart the agent: it then reads the new ISM. To change an ISM, remove the domain and add it again. |
| Controller removes the domain | The daemon stops the domain. |

## Ports

| Port | Who reads it | Endpoints |
|---|---|---|
| 9000 | Any relayer | S3-compatible anonymous `GET` and `HEAD`. Open it to everyone. |
| 9003 | Monitoring | `GET /health`, `GET /status`. Keep it on a private address. |

Hyperlane relayers are permissionless, so many relayers can read port 9000.
The data is public: signatures, roots, indexes, message ids and the
announcement. The process on port 9000 also holds the signing key. A reverse
proxy (nginx, caddy) with a request limit in front of port 9000 is
recommended. A signed checkpoint never changes, so the proxy can cache it.

Port 9000 uses path-style requests: `GET /<bucket>/<domain>/<key>`.

| Key | Body |
|---|---|
| `checkpoint_latest_index.json` | The latest signed index, a JSON number. |
| `checkpoint_<index>_with_id.json` | The signed checkpoint, the same bytes as the upstream agent. |
| `announcement.json` | The signed announcement, pretty JSON. |
| `reorg_flag.json` | Exists only while the domain is halted after a reorg: `{"reason": "…"}`. Relayers stop the deliveries from that origin while one validator serves it. |

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
- The relayer stopped the deliveries of a domain (`relay.error`).

While the daemon waits for the controller, `/health` returns 200.

`GET /status` fields:

| Field | Meaning |
|---|---|
| `healthy` | The `/health` result. |
| `controller` | `unknown`, `waiting_for_controller` or `ready`. |
| `active` | `V` is in the active validator set. `null` before the first read. |
| `signer_error` | Why `H` is not the accepted checkpoint signer, or `null`. |
| `miss_count` | `missCount(V)` on the controller. |
| `relayer.delivered` | `process` transactions of this relayer that a block included, since the start. |
| `relayer.last_delivery` | Origin domain, leaf index, destination domain, transaction hash and Unix time of the last one, or `null`. |
| `submitted`, `failed` | `submitReport` counters since the start. `submitted` counts only transactions with a successful receipt. A revert or no receipt is a failure. |
| `last_submit_time` | Unix time of the last successful `submitReport` receipt. `0` before the first one. |
| `last_submit_error` | The error of the last failed `submitReport`, or `null` after a success. |
| `domains.<id>.leaves` | Stored leaf count. |
| `domains.<id>.latest_signed_index` | Highest signed index. |
| `domains.<id>.cursor` | Finalized origin block of the last stored leaf batch. It is `null` before the first leaf. It moves only when new leaves arrive, so it can stay behind the chain head while the domain has no messages. |
| `domains.<id>.head` | The last final block of the origin chain. `null` before the first read. |
| `domains.<id>.behind` | Blocks up to `head` that the sync did not read yet. `0` means that the sync is at the head. |
| `domains.<id>.balance_wei` | native balance of `H` on this chain in wei, as a string. `H` pays the gas of each delivery to this chain. `null` before the first read. |
| `domains.<id>.relay.cursor` | the next new leaf index that the relayer takes in. `null` before the relayer starts on the domain. |
| `domains.<id>.relay.backlog` | older leaves below this index wait for the take-in. `0` means that the history is done. |
| `domains.<id>.relay.queue` | messages that are not delivered yet. |
| `domains.<id>.relay.dropped` | messages after the last delivery attempt, and messages that can never be delivered. |
| `domains.<id>.relay.error` | why the relayer stopped the deliveries of this domain, or `null`. |
| `domains.<id>.report` | The last liveness report of this validator that the controller accepted: `round`, and the validators that it named in `not_serving` and `not_delivered`. `null` before the first one in this process. |
| `domains.<id>.reorg` | The domain halted after a reorg. |
| `domains.<id>.error` | Domain error, or `null`. |

The `status` command prints `/status` of the running agent as a table:

```text
$ outbe-hyperlane-agent status
healthy: true   controller: ready   active: true   misses: 0
delivered: 12

domain         leaves   signed        head  behind  queue  dropped       balance  state
54322345         1204     1203       48220       0      0        0       12.5000  ok
97                388      387    61002113       2      1        0        0.0412  syncing
```

`balance` is the native balance of `H` on that chain. Add funds before it is
zero: without gas, the relayer cannot deliver to that chain and the
validator cannot announce.

It reads `[health] bind_address` from the configuration. Use `--url` for
another address.

## Storage

`[storage] db_path` is the directory of the validator's own MDBX store. The
store opens in exclusive mode. While the daemon runs, no other process can
open it.

| Table | Key | Value |
|---|---|---|
| `Leaves` | (domain, index) | Message id, origin block |
| `Checkpoints` | (domain, index) | Root, 65-byte signature |
| `Cursor` | domain | Finalized origin block of the last stored leaf batch |
| `Meta` | (domain, name) | `Announcement`, `Signer`, `ReorgFlag`, `Hook`, `RelayCursor`, `RelayBackCursor` |
| `Relay` | (domain, index) | A message that the relayer did not deliver yet: message bytes, first-seen time, attempt count, time of the next attempt, dropped flag |

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
| `submitReport` | Skipped. The controller would reject it. |
| Relayer | Sends no transaction. It deletes a message from its queue when another relayer delivers that message. |
| `/status` | `"active": false`. This is not an error. |

While `V` is active, the daemon sends one `submitReport` for each running
domain in each epoch. It sends the call in the last quarter of the epoch.
A validator that is behind thus has three quarters of the epoch to sign the
target index. The daemon reads the start and the length of the epoch from the
`ValidatorSet`. `poll_interval_secs` must be much shorter than a quarter of
the epoch: an agent that does not poll in that quarter loses its vote.

The call has these parts:

- Liveness vote: the root of the checkpoint that the daemon signed at
  `targetIndex(domain)`. The call does not have the signature: the other
  validators read it from the storage of this daemon (see `notServing`). The
  controller compares the roots of all validators at the target index. A
  missing vote, or a root that differs from the root of the quorum, is a
  liveness miss. When there is no target, or the store does not have that
  index yet, the call has the root at the latest signed index.
- Report: the latest signed index. The controller sets the next target from
  these indexes.
- Report: `notServing`, the other active validators that do not serve their
  signed checkpoint at the target index. The daemon reads the checkpoint from
  the announced storage of each validator. A validator goes to the list when
  the read fails, when another key signed the checkpoint, or when the root is
  not the root of this daemon. When no validator answers, the daemon names
  nobody: the cause can be its own network.
- Report: `notDelivered`, the active validators that did not deliver a message
  in their turn. The check starts at `deliveryIndex(domain)` of the
  controller and goes up to the target index. For each validator, the daemon
  looks at the first message whose first turn that validator has. It reads
  `processor(id)` and `processedAt(id)` of the destination Mailbox at a final
  block. The validator goes to the list only when another active validator
  delivered the message later than 40 seconds after the message was ready.
  The message is ready at the time of the origin block that makes its block
  final (`reorg_period` blocks later). Only chain data decides, so every
  daemon gets the same result. An origin with a finality tag has no delivery
  check.

The two lists hold 23 addresses together. A validator that a quorum names in
a list gets a liveness miss. A validator that gave a report and did not name
a validator that a quorum named in `notServing` gets a miss too. `notDelivered`
has no such rule: the delivery check uses `reorg_period` of the local
configuration, so two honest daemons can get different results. When a check did not run, the daemon tries
again on the next poll. In the last eighth of the epoch it sends
the call with no names, so the vote is not lost.

The daemon sends no call when `reportedRound(V, domain)` is the current
`livenessRound()`: the controller has the report of this round. It also sends
no call that gives the controller no new vote and no newer index. The daemon
never submits index 0. It waits up to 60 seconds for the receipt.

The controller gives the liveness verdict in the first block of each epoch.

## Relayer

The daemon starts one relayer task for each domain. The task delivers the messages of that origin chain to the other
domains of the `HyperlaneController`. A relayer error does not stop the
validator role.

The relayer starts when the validator sync is at the head of the chain. As
the upstream relayer, it starts at the newest leaf and goes in two
directions: new messages first, then the older messages from the newest to
the oldest. The history of a chain does not delay a new message.

For each message, the relayer does these steps:

1. **Take in.** The validator sync stores the message id and the block of
   each leaf. The relayer reads the `Dispatch` log of that block and accepts
   a message only when the hash of the message is the stored leaf. It writes
   the message to the `Relay` table.
2. **Check.** It reads `delivered(id)` on the destination Mailbox at a final
   block. When the result is true, it deletes the row.
3. **Wait for the turn.** See the delivery order below.
4. **Check the recipient.** A recipient without code on the destination
   chain can never get the message: the relayer drops the message at once.
5. **Read the ISM.** It reads `recipientIsm(recipient)` and then
   `validatorsAndThreshold(message)`. The ISM must be
   `HyperlaneController.ismByDomain(destination)`, also behind routing ISMs.
   The recipient contract names its ISM, so without this rule any sender
   could give the relayer a validator set of its own. A message with another
   ISM is dropped at once.
6. **Read the signatures.** It reads `checkpoint_<index>_with_id.json` from
   the newest announced location of each ISM validator, its own location
   included. It reads the validators in batches of 1.5 times the threshold
   (10 at most) and stops when one root has the threshold of signatures.
   When one validator serves a JSON object at `reorg_flag.json`, the relayer
   does not deliver.
7. **Send.** It sends `process(metadata, message)` to the destination
   Mailbox. The gas limit is the estimate plus 75,000. `H` pays the gas. A
   delivery with an estimate above 3,000,000 gas is not sent.

Delivery order. Every validator runs a relayer, so the relayers take turns:

- For each message, the relayer sorts the active validators by
  `keccak256(message id || validator address)`. Every relayer gets the same
  order. The order is different for each message.
- The first validator of the order sends at once.
- Each next validator waits 40 seconds more, and sends only when the
  message is still not delivered. The 40 seconds are a constant of the agent,
  the same for every validator and every chain.
- The relayers do not talk to each other. Each one reads `delivered(id)`
  before it sends.

Example with four validators: the first relayer
sends at 0 s, the second at 40 s, the third at 80 s, the fourth at 120 s.
When the first relayer works, the others send nothing.

| Rule | Detail |
|---|---|
| Gas | `H` needs native gas on every destination chain. |
| Finality | A row goes away only when `delivered(id)` is true at a final block (`reorg_period` of the destination). A reorg of the destination cannot lose a message. |
| Retry | A failed attempt follows the upstream schedule: 5 s, 10 s, 30 s, 60 s, then 3 minutes, and longer pauses later. Missing signatures get three fast retries of 2 s. Every error counts as a failed attempt, also an RPC error. The relayer then reads from the next RPC endpoint. |
| Order | Messages with no failed attempt go first, then the retries by the time of their next attempt. |
| Dropped | After 66 attempts (about two weeks) the row stays as dropped. `/status` counts it. Anyone can still deliver the message by hand. |
| One transaction at a time | The relayer sends one `process` transaction for each chain and waits up to 90 seconds for the receipt. On one chain, the relayer, the liveness and the announce never read a nonce at the same time. |
| Gas price | EIP-1559 fees. On a chain with no base fee or a zero base fee, a legacy transaction with the gas price of the node. |
| Fee cap on Outbe | A delivery to Outbe has a fee cap of 10 base fees. It pays the base fee of its block, not the cap. A delivery that stays in the pool also stops every `submitReport` of `H` behind it, because `H` has one nonce sequence. ZeroFee removes the fee, not the place in that sequence. |
| Storage locations | Only `s3+http://` and `s3+https://` locations. Only status 200 is an object. No redirect, a 10 second timeout, a 50 KiB body. An ISM validator chooses its location: the relayer does not refuse a private address. |
| Largest delivery | 3,000,000 gas. There is no gas payment check yet, so `H` pays each delivery, and `H` also pays the liveness transactions. |
| ISM limits | At most 13 ISMs in a row. The validator set must have 1 to 50 validators and a threshold above 0. |
| Payment | The relayer does not check the IGP payment. It delivers every message that the ISM accepts. |

## Limits

| Limit | Detail |
|---|---|
| First sync memory | Unverified leaves stay in memory until the vote verifies them. Expect about 1 GB per 5M leaves on the first sync. |
| Outbe `eth_getLogs` | The Outbe RPC rejects ranges above 100,000 blocks. The sync uses at most 100,000 and halves the range after a range error. |
| `metadata_latest.json` | Not served. The relayer does not read it. |
| Relayer history | The relayer takes in the older messages after the new ones, 32 leaves for each poll and only while fewer than 32 messages are in the queue: one `delivered` call for each old message. An old leaf whose `Dispatch` log no RPC endpoint of the chain has is passed with an error in the log. |
| Relayer gas price | No gas price escalation. A transaction that stays in the pool stops the deliveries to that chain until a block includes it. |
| Relayer ISM | Only the multisig ISM of the HyperlaneController for the destination domain, also behind routing ISMs. A recipient with its own ISM gets no delivery from this relayer: its owner must run a relayer. |
| Dropped rows | The relayer reads `delivered(id)` for a dropped row once an hour and removes the row when someone delivered the message. |
| Relayer differences from upstream | No cache of contract reads and checkpoints. No gas payment check. The delivery is final at a final block of the destination (`reorg_period`), not after a fixed 10 minutes. The Classic submit path, not Lander. |
| Private key | The key is plaintext in the config file. |
