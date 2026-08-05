# Running a full node and a validator

There are two node roles:

- **Full node** — `outbe-chain node` (no `--validator`). It has a persistent
  compressed Reth P2P identity registered through TeeRegistry V1, follows a
  selected certified `--upstream`, re-executes blocks and serves RPC. It does not
  vote or propose and needs no consensus BLS key.

- **Validator** — `outbe-chain node --validator`. This is **one** role with a
  lifecycle. The node always runs `--validator`; what it does depends on whether it
  currently holds a BLS threshold share:
  - **No share yet** — it follows finalized blocks through the consensus mesh as a
    share-less _verifier_ (the code calls this a "finalized-follower"): it syncs,
    processes offers, and survives DKG rotations, but cannot vote. **This is a
    transient lifecycle phase, not a separate role** — a node is here only while it
    is waiting for its first share, or while a restarted node catches up. You do not
    stay here permanently.
  - **Has a share** — it is an ACTIVE signer: it proposes and votes.

On-chain PoS status tracks the same lifecycle: REGISTERED → PENDING → **ACTIVE** →
EXITING → UNBONDING → INACTIVE, with JAILED as the punishment/recovery branch.
Validators that still hold a BLS share vote and remain accountable in the current
committee: ACTIVE, EXITING, and temporarily JAILED until the next reshare clears
that share.

> **The TEE enclave is mandatory for every V1 chain and every node role.** The
> ChainSpec must contain `teeAttestationV1`, and startup requires
> `--tee-enclave-socket`. A node that does not hold the exact permanent offer key
> cannot participate in consensus or execute canonical transactions. There is no
> in-process stub, tee-less chain or offer-free exception. DKG consensus key
> material is separate and lives in `--consensus.keys-dir`, not in the enclave.

> **Evidence boundary.** The reachable restart path is exercised on the isolated
> `GramineDirectDev` mock localnet and proves development behavior only. Deterministic
> tests cover Validator and FullNode V1 admission and fail-closed startup seams.
> Real exact-release `DcapRequired` Validator and FullNode paths remain a
> mandatory I9 hardware gate. A logical or physical 32-validator network is
> not required.

---

## 0. Prerequisites

```sh
cargo build --release -p outbe-chain  --bin outbe-chain    # the node
cargo build --release -p outbe-cli    --bin outbe-cli      # operator CLI
cargo build --release -p outbe-keygen --bin outbe-keygen   # key generation
```

- The genesis bundle: `genesis.json` with mandatory `teeAttestationV1`, the Reth bootnode list, and — for a validator
  (it joins the consensus mesh) — the network's **public** DKG artifacts
  `polynomial.hex` and `dkg-output.hex` (public, no secret share).
- The `outbe-tee-enclave` sidecar (the exact accepted `gramine-sgx` release in
  production; an explicit GramineDirectDev binary on localnet). It must be running
  before `tee join` or node startup.
- For testnet, deploy the exact Cosign-verified enclave image digest and compare its
  signed ReleaseManifest measurements by following
  [Testnet SGX release and rollout](testnet-sgx-release.md). Do not build or sign the
  release bundle on the validator host.
- An EVM account (secp256k1) funded with native COEN.

`outbe-cli` / `outbe-keygen` never send key material to the RPC; only signed
transactions and public keys go over the wire.

---

## 1. Full node (sync + RPC only)

Run the node **without** `--validator`. It must first register its persistent Reth
P2P identity and install the one-time permanent-key artifact. The node data
directory passed to `tee join` must be the same resolved chain-specific directory
that `outbe-chain` uses. The P2P secret file must also be the same one used by the
node.

```sh
RETH_P2P_SECRET=/var/lib/outbe/reth-p2p-secret.hex
NODE_DATA_DIR=/var/lib/outbe/<resolved-chain-data-dir>
BINDING_ID=0x$(openssl rand -hex 32)
VALID_UNTIL=$(( $(date -u +%s) + 86400 ))

# The exact release enclave is already running at 127.0.0.1:7000.
# RELAY_EVM_KEY is only a funded transaction relay; the P2P key is node authority.
outbe-cli tee join --enclave-socket 127.0.0.1:7000 \
  --profile full-node \
  --node-data-dir "$NODE_DATA_DIR" \
  --reth-p2p-secret-key "$RETH_P2P_SECRET" \
  --binding-id "$BINDING_ID" --valid-until "$VALID_UNTIL" \
  --private-key "$RELAY_EVM_KEY" \
  --rpc-url http://<certified-rpc>:8545 --timeout-secs 60

outbe-chain node \
  --chain /path/to/genesis.json --datadir /var/lib/outbe \
  --p2p-secret-key "$RETH_P2P_SECRET" \
  --bootnodes "<enode URLs>" \
  --upstream http://<selected-certified-upstream>:8545 \
  --consensus.storage-dir /var/lib/outbe/consensus \
  --ocomp.supervisor-socket /opt/outbe-chain/ocomp/run/node-supervisor.sock \
  --ocomp.snapshot-exporter-socket /opt/outbe-chain/ocomp/run/node-snapshot-exporter.sock \
  --ocomp.supervisor-uid "$(id -u outbe-ocomp-supervisor)" \
  --ocomp.snapshot-exporter-uid "$(id -u outbe-ocomp-export)" \
  --ocomp.protocol-bundle-hash <0x-protocol-bundle-hash> \
  --ocomp.boot-nonce <nonzero-0x32-byte-boot-nonce> \
  --http --http.addr 127.0.0.1 --http.port 8545 --http.api eth,net,web3,outbe \
  --tee-enclave-socket 127.0.0.1:7000
```

`tee join` must finish successfully before the FullNode process starts. At
startup the node reads the upstream canonical 32-byte offer key, requires a
ready nonzero resident key and compares them exactly before launching Reth.
The six `--ocomp.*` control arguments are an all-or-nothing FullNode profile;
do not pass `--ocomp.key`. Run the normal SnapshotExporter/worker services and
`outbe-ocomp follower` with the same `OCOMP_CHAIN_ID`, `OCOMP_GENESIS_HASH`,
`OCOMP_BOOT_NONCE`, `OCOMP_PROTOCOL_BUNDLE_HASH`, `OUTBE_OCOMP_BASE_PATH` and
`OUTBE_OCOMP_NODE_USER` deployment identity. The follower performs Lysis and
durably publishes the exact local result, but it never reads an OCOMP signing
key or `OUTBE_OCOMP_RPC_URL` and never submits a vote.

> **RPC exposure.** Examples bind RPC to `127.0.0.1` and enable only the
> `eth,net,web3,outbe` modules. Never add `admin` or `debug` to `--http.api` on a
> public (`0.0.0.0`) binding: that exposes unauthenticated node control. To serve
> RPC off-host, put it behind authentication or a firewall that restricts access
> to trusted operators, or keep `admin`/`debug` on a local IPC socket. The
> `--consensus.listen-addr 0.0.0.0:30400` P2P port below is the consensus gossip
> listener and is meant to be reachable by peers.

Check it with `outbe-cli monitor health` / `cast block finalized`. A certified
FullNode's `outbe_consensusStatus.lastFinalizedBlock` advances only after the
exact parent proof, OCOMP retention and snapshot arming have all succeeded.

If this FullNode later becomes a validator, keep the synchronized data directory,
stop it, and restart it with the complete validator profile from sections 2–3.

---

## 2. Becoming a validator

A validator always runs `outbe-chain node --validator`. One-time setup, then run the
node; it joins as a share-less follower and becomes a voting signer at a DKG reshare.

### 2.1 Keys

```sh
outbe-keygen hybrid --output-dir /var/lib/outbe/keys
# writes signing-key.hex (BLS12-381) + evm-key.hex (secp256k1)

BLS_PUBKEY=$(outbe-keygen show-pubkey --key /var/lib/outbe/keys/signing-key.hex \
  | grep -oE '[0-9a-f]{96}' | head -1)
EVM_KEY=0x$(tr -d '[:space:]' < /var/lib/outbe/keys/evm-key.hex)
VALIDATOR_ADDR=$(cast wallet address --private-key "$EVM_KEY")
```

Keep `signing-key.hex` / `evm-key.hex` secret and backed up.

Generate the validator's OCOMP result-signing key before its first validator
startup. The public registration binds this key to the chain, genesis,
validator address and current BLS MinPk identity; the secret key remains local:

```sh
outbe-keygen ocomp --output-dir /var/lib/outbe/ocomp \
  --chain-id <chain-id> --genesis-hash <0x-genesis-hash> \
  --validator-address "$VALIDATOR_ADDR" \
  --consensus-bls-min-pk "$BLS_PUBKEY"
```

This writes `ocomp-key-v1.hex` and the public
`ocomp-registration-v1.ocb1`. A validator startup without the OCOMP key and
complete local-control configuration is rejected. A FullNode needs no OCOMP
signing key or registration, but it does require the complete keyless
local-control profile described in section 1.

### 2.2 Register, announce P2P, and install the offer key once

```sh
# register the validator (binds your address to your BLS pubkey) -> REGISTERED
SIG=$(outbe-keygen sign-registration --key /var/lib/outbe/keys/signing-key.hex \
        --validator-address "$VALIDATOR_ADDR" | grep -oE '[0-9a-f]{120,}' | head -1)
outbe-cli validator register --pubkey "0x$BLS_PUBKEY" --bls-sig "0x$SIG" \
  --private-key "$EVM_KEY" --rpc-url http://<rpc>:8545

# publish your consensus P2P address (the mesh reads it from chain state)
outbe-cli validator set-p2p --symmetric <public-host>:30400 \
  --private-key "$EVM_KEY" --rpc-url http://<rpc>:8545

# Start the exact accepted enclave sidecar first. Then bind its V1 identity and
# install the permanent key before running this joining node.
NODE_DATA_DIR=/var/lib/outbe/<resolved-chain-data-dir>
BINDING_ID=0x$(openssl rand -hex 32)
VALID_UNTIL=$(( $(date -u +%s) + 86400 ))
outbe-cli tee join --enclave-socket 127.0.0.1:7000 \
  --profile validator \
  --node-data-dir "$NODE_DATA_DIR" \
  --node-private-key "$EVM_KEY" \
  --consensus-bls-public "0x$BLS_PUBKEY" \
  --binding-id "$BINDING_ID" --valid-until "$VALID_UNTIL" \
  --private-key "$RELAY_EVM_KEY" \
  --rpc-url http://<certified-rpc>:8545 --timeout-secs 60
```

The relay and validator keys may belong to different accounts; admission authority
comes from the Validator NodeHost signature and canonical evidence, not
`msg.sender`. A Created registration emits one matching
`OfferKeySealedForRegistryV1`; idempotent replay, renewal and replacement never
redeliver it.

Registration admits your node to the consensus mesh as a non-voting peer; it does
not by itself make you a voting validator (no stake, no share yet). The non-voting
admission tier also includes PENDING and JAILED validators so they can sync,
recover, and rejoin.

### 2.3 Run the node (`--validator`)

Launch with `--validator`, the public DKG artifacts (to verify finality), the
enclave socket, and your keys — but **no** `--consensus.signing-share` (you have no
share yet). The node runs the consensus engine as a share-less follower:

```sh
outbe-chain node --validator \
  --chain /path/to/genesis.json --datadir /var/lib/outbe \
  --bootnodes "<enode URLs>" \
  --http --http.addr 127.0.0.1 --http.port 8545 --http.api eth,net,web3,outbe \
  --consensus.signing-key       /var/lib/outbe/keys/signing-key.hex \
  --validator.evm-key           /var/lib/outbe/keys/evm-key.hex \
  --consensus.public-polynomial /path/to/polynomial.hex \
  --consensus.dkg-output        /path/to/dkg-output.hex \
  --consensus.listen-addr       0.0.0.0:30400 \
  --consensus.peers             "<bls_pubkey>@<host:port>,..." \
  --ocomp.supervisor-socket     /run/outbe/ocomp-supervisor.sock \
  --ocomp.snapshot-exporter-socket /run/outbe/ocomp-snapshot-exporter.sock \
  --ocomp.supervisor-uid        <supervisor-uid> \
  --ocomp.snapshot-exporter-uid <snapshot-exporter-uid> \
  --ocomp.protocol-bundle-hash  <0x-chain-pinned-bundle-hash> \
  --ocomp.boot-nonce            <fresh-nonzero-0x32-byte-value> \
  --ocomp.key                   /var/lib/outbe/ocomp/ocomp-key-v1.hex \
  --tee-enclave-socket          127.0.0.1:7000
```

There is deliberately no OCOMP validator-index or committee file. For every
finalized job the node opens that job's historical ValidatorSet snapshot,
finds `VALIDATOR_ADDR`, checks that the stored OCOMP public key matches
`ocomp-key-v1.hex`, and signs with the index from that snapshot. A membership
change therefore affects only new jobs; old live jobs keep their old index and
quorum.

The two OCOMP sockets belong to the local Supervisor and SnapshotExporter
processes. Their peer UIDs, the bundle hash and the boot nonce form one
all-or-nothing startup profile. The OCOMP key file and the node-owned sign-once
journal under the chain data directory must survive restarts. An exact retry
returns the byte-identical stored signature; a changed job binding is refused
as equivocation.

> The node sources its DKG `prev_output`/polynomial from the **chain** (the latest
> finalized DKG boundary), so the public artifact files only need to be valid genesis
> material — the node adopts the committee's current output automatically.

Wait until it is caught up to the finalized tip:

```sh
cast rpc outbe_syncStatus --rpc-url http://localhost:8545
outbe-cli monitor readiness --rpc-url http://localhost:8545
```

### 2.4 Stake (→ PENDING)

```sh
outbe-cli staking stake --validator "$VALIDATOR_ADDR" --amount <amount> \
  --private-key "$EVM_KEY" --rpc-url http://<rpc>:8545
```

Staking accumulates; once your **cumulative** stake reaches `min_stake`, you move
REGISTERED → **PENDING**. (A smaller stake is accepted, it just leaves you REGISTERED
until the total reaches `min_stake`.)

### 2.5 Confirm readiness (→ eligible) and wait for the reshare (→ ACTIVE)

A PENDING validator is not admitted to the next reshare until it confirms,
on-chain, that it has caught up and supplies the canonical proof-of-possession
registration for its OCOMP result key. Only after the node has reached the
finalized tip, submit the public registration generated in section 2.1:

```sh
outbe-cli validator confirm-ready \
  --registration /var/lib/outbe/ocomp/ocomp-registration-v1.ocb1 \
  --private-key "$EVM_KEY" --rpc-url http://<rpc>:8545
```

DKG reshares are **periodic** (one per epoch, height-driven). At the first reshare
after you are confirmed, the ceremony grants your node a share and promotes you
PENDING → **ACTIVE** (`hasBLSShare = true`) exactly at the epoch boundary. Your
running node completes the ceremony ("threshold material obtained"), switches to
signer mode, and votes in lockstep. There is no on-demand "join now" — you wait for
the next periodic reshare (≤ one epoch). Confirm:

```sh
cast call 0x000000000000000000000000000000000000EE00 \
  'isConsensusParticipant(address)(bool)' "$VALIDATOR_ADDR" --rpc-url http://<rpc>:8545   # true
outbe-cli validator participation --rpc-url http://<rpc>:8545   # watch it vote in lockstep
```

---

## 3. The two onboarding situations

Both run `outbe-chain node --validator` with the same `--datadir`/`--consensus.keys-dir`;
the difference is only whether the node already holds a share.

- **New validator (first time)** — no share on disk. The node comes up as a share-less
  follower (section 2.3), syncs to head, and once you have staked + confirmed it
  **waits for the next periodic DKG reshare** to be granted a share and become ACTIVE.

- **Returning validator (was ACTIVE)** — restart `node --validator` with its existing
  `--consensus.keys-dir`. It **recovers its share from disk**, catches up to head, and
  **resumes signing without a new reshare** while its on-chain status is still ACTIVE.
  If it has already left the active set and reached INACTIVE, it must complete the
  normal re-registration/stake/PENDING flow and submit `confirm-ready` again; exit
  clears the prior OCOMP readiness. It then rejoins only through the next certified
  reshare.

- **FullNode becoming a validator** — stop the FullNode and restart the same
  synchronized data directory with `--validator`, the consensus/EVM/OCOMP keys,
  public DKG artifacts and the complete OCOMP local-control profile from section
  2.3. It does not receive a manually assigned OCOMP index. After registration,
  stake and `confirm-ready`, the next certified reshare makes it ACTIVE and its
  address appears automatically in snapshots for new OCOMP jobs.

Once a new or returning node is registered, staked, PENDING and confirmed, the
normal action is simply to **wait for the next periodic reshare** — it re-reads the
eligible set each epoch and picks the validator up. There is no on-demand or forced
DKG path. In particular, losing the permanent enclave offer key never starts DKG or
reshare: that NodeId is lost and cannot receive the key again. Continuing requires
an independently admitted new node identity; the old identity cannot be recovered
or replaced.

If an old job's pinned snapshot is missing/evicted, the local validator is absent
from it, or its stored OCOMP key does not match, the node abstains from that job.
It logs the JobId and all three snapshot bindings and increments
`outbe_ocomp_attestation_abstentions_total`; it never substitutes the current
ValidatorSet. Other consensus and FullNode processing continues.

---

## 4. Validator statuses

`validatorByAddress(addr)` on ValidatorSet (`0x…EE00`) returns the status code:

| Code | Status     | Meaning                                                                                                                                     |
| ---- | ---------- | ------------------------------------------------------------------------------------------------------------------------------------------- |
| 0    | REGISTERED | registered (+ usually P2P-announced + enclave-joined); not staked; non-voting follower                                                      |
| 1    | PENDING    | staked, awaiting confirm-ready + the reshare that grants a share (excluded from `activeValidatorCount`)                                     |
| 2    | ACTIVE     | holds a share; voting                                                                                                                       |
| 3    | EXITING    | left the active set; still accountable (keeps signing) until the next reshare excludes it                                                   |
| 4    | UNBONDING  | excluded by a reshare; share cleared; stake unbonding                                                                                       |
| 5    | INACTIVE   | unbonding complete; stake withdrawn                                                                                                         |
| 6    | JAILED     | punished on a felony (slashed + frozen); dropped from the committee at the next reshare, but kept in the registry pending unjail or unstake |

### Felony → JAILED

On a consensus/oracle **felony** (proposer/voter miss threshold, double-proposal /
conflicting-vote / invalid-VRF evidence, or oracle underperformance) the validator
is **slashed and moved to JAILED** — not force-exited out of the registry. It keeps
current-epoch accountability until the next reshare clears its share (like EXITING),
then it stops voting; it stays admitted to P2P as a non-voting follower so it keeps
syncing. From JAILED there are two ways out:

- **Return:** top up your stake to at least `min_stake` if the slash dropped you
  below it, then send `outbe-cli staking unjail` (caller = the validator). This
  moves you JAILED → PENDING; then `confirm-ready` and the next reshare promote you
  back to ACTIVE. A stake top-up alone does **not** unjail — the explicit unjail tx
  is always required. (An optional cooldown, `config_unjail_cooldown_blocks`, gates
  how soon you may unjail; default 0.)
- **Leave:** unstake your full stake — from JAILED this enters the
  EXITING → UNBONDING → INACTIVE drain, and you are no longer a validator.

---

## 5. Leaving the active set

```sh
outbe-cli validator deactivate --private-key "$EVM_KEY" --rpc-url http://<rpc>:8545
```

Moves you ACTIVE → **EXITING** immediately; you keep signing until the next reshare,
which excludes you (EXITING → **UNBONDING**, share cleared). Your node then
**transitions to the share-less follower phase** of the smaller committee — it stays online
following finality rather than shutting down. Stop the process to leave entirely.

Unstake / withdraw:

```sh
outbe-cli staking unstake --amount <amount> --private-key "$EVM_KEY" --rpc-url http://<rpc>:8545
# after the unbonding period:
outbe-cli staking claim --private-key "$EVM_KEY" --rpc-url http://<rpc>:8545
```

Unstaking below `min_stake` from ACTIVE also triggers EXITING → UNBONDING; from
PENDING it reverts to REGISTERED.

---

## 6. Restart and recovery

The DKG share is persisted to `--consensus.keys-dir` (default `<datadir>/keys`); a
restart recovers it and resumes signing without a new reshare (the "returning
validator" case in section 3). Restart with the same `--datadir`/`--consensus.keys-dir`.

Consensus recovery is fail-closed. A restarted validator decides which epoch
committee it may sign for from durable consensus finalization and DKG boundary
evidence. The latest local execution head alone is insufficient. If the local Reth
head is only in the normal bounded in-flight window ahead of the marshal-finalized
tip, and that head includes an unfinalized membership change, an old-epoch signer
can still recover from the finalized DKG boundary and continue signing until the
activation is actually finalized.

Stop and investigate rather than deleting files if startup reports missing marshal
finalization, inconsistent saved/pending DKG material, a pending boundary snapshot
without matching DKG material, an EVM key that does not match the recovered boundary
address for the local BLS key, or execution history with no durable consensus
finalization evidence. For details and operator actions, see
[`consensus-restart-recovery.md`](consensus-restart-recovery.md).

For the enclave, production requires sealing (`--tee-dir <path>` + `--chain-id`) and
restores the exact same permanent offer key. Missing, corrupt, wrong-chain or otherwise
unsealable permanent-key state is terminal for that identity; there is no recovery,
replacement, forced DKG or fallback. The separate GramineDirectDev chain may use an
explicitly ephemeral enclave only for fresh development networks and is never a restart
fallback for production.

---

## 7. Reference

### Protocol addresses

| Precompile   | Address                                      |
| ------------ | -------------------------------------------- |
| ValidatorSet | `0x000000000000000000000000000000000000EE00` |
| Staking      | `0x000000000000000000000000000000000000EE02` |
| TeeRegistry  | `0x000000000000000000000000000000000000EE0A` |

### Key node flags

| Flag                                                       | Purpose                                                                                                                                         |
| ---------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------- |
| `--validator`                                              | run the consensus thread (validator); omit for a full node (EL sync + RPC only)                                                                 |
| `--consensus.signing-key` / `--validator.evm-key`          | BLS signing key / secp256k1 system-tx signer (validator)                                                                                        |
| `--consensus.signing-share`                                | BLS threshold share — present only once the node holds a share                                                                                  |
| `--consensus.public-polynomial` / `--consensus.dkg-output` | public DKG artifacts to follow finality before holding a share                                                                                  |
| `--consensus.keys-dir`                                     | where the DKG share/polynomial/output are persisted (default `<datadir>/keys`)                                                                  |
| `--consensus.listen-addr` / `--consensus.peers`            | consensus P2P listen address / bootstrap hint `<bls_pubkey>@<host:port>`                                                                        |
| `--ocomp.supervisor-socket` / `--ocomp.snapshot-exporter-socket` | local Supervisor/Follower and SnapshotExporter endpoints; both are mandatory together on validators and certified FullNodes              |
| `--ocomp.supervisor-uid` / `--ocomp.snapshot-exporter-uid` | expected Unix peer identities for those two local endpoints                                                                                    |
| `--ocomp.protocol-bundle-hash` / `--ocomp.boot-nonce`      | chain-pinned OCOMP bundle identity / nonzero per-boot control-session binding                                                                   |
| `--ocomp.key`                                              | validator-only node-owned OCOMP result-signing key registered through `confirm-ready`; omit on FullNode; no static participant index is configured |
| `--tee-enclave-socket`                                     | mandatory enclave sidecar endpoint; every V1 node fails startup if it is absent or cannot satisfy its genesis-fixed mode                        |
| `--testnet.trust-el-head`                                  | disaster-recovery only: trust execution head when no durable consensus-finalized height exists (testnet/devnet; not normal production recovery) |

### Operator commands

| Command                                                     | Purpose                                                        |
| ----------------------------------------------------------- | -------------------------------------------------------------- |
| `outbe-keygen hybrid` / `show-pubkey` / `sign-registration` | generate keys / derive BLS pubkey / sign registration          |
| `outbe-keygen ocomp`                                       | generate the permanent validator OCOMP key and public `ocomp-registration-v1.ocb1` artifact |
| `outbe-cli tee join`                                        | register an exact Validator or FullNode identity and ingest its one-time permanent-key artifact through any funded relay |
| `outbe-cli validator register` / `set-p2p`                  | register (→ REGISTERED) / publish the P2P address              |
| `outbe-cli staking stake` / `unstake` / `claim`             | stake (→ PENDING at `min_stake`) / unstake / withdraw          |
| `outbe-cli staking unjail`                                  | return a JAILED validator → PENDING (stake ≥ min_stake)        |
| `outbe-cli validator confirm-ready --registration <file>`   | confirm caught-up and register the OCOMP result key            |
| `outbe-cli validator deactivate`                            | leave the active set (→ EXITING)                               |
| `outbe-cli monitor health` / `readiness` / `watch`          | health / readiness / dashboard                                 |
| `outbe-cli validator participation` / `list` / `info`       | participation + set inspection                                 |

---

## Localnet quickstart

The development validator path runs through the Rust/Cucumber harness on an
isolated four-validator GramineDirectDev mock localnet. This is never hardware
evidence:

```sh
mise run e2e
```

The harness owns the node processes, enclave containers, port ranges, data
directories, and a temporary MongoDB replica set. See
`testing/e2e-harness/README.md` for focused feature commands and debug
options.
