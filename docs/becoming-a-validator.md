# Running a full node and a validator

There are two node roles:

- **Full node** — `outbe-chain node` (no `--validator`). Syncs the chain over the
  execution devp2p network, re-executes blocks, serves RPC. No consensus thread,
  no BLS key, not in the consensus mesh, not registered on-chain. Trusts finality
  (`phase1VerificationMode = trustedFinality`). It does not vote or propose.

- **Validator** — `outbe-chain node --validator`. This is **one** role with a
  lifecycle. The node always runs `--validator`; what it does depends on whether it
  currently holds a BLS threshold share:
  - **No share yet** — it follows finalized blocks through the consensus mesh as a
    share-less *verifier* (the code calls this a "finalized-follower"): it syncs,
    processes offers, and survives DKG rotations, but cannot vote. **This is a
    transient lifecycle phase, not a separate role** — a node is here only while it
    is waiting for its first share, or while a restarted node catches up. You do not
    stay here permanently.
  - **Has a share** — it is an ACTIVE signer: it proposes and votes.

On-chain PoS status tracks the same lifecycle: REGISTERED → PENDING → **ACTIVE** →
EXITING → UNBONDING → INACTIVE. Only ACTIVE (and EXITING until the next reshare)
hold a share and vote.

> **The TEE enclave is required to process tribute offers.** Offers are encrypted
> to the chain's offer key and decrypt **only inside the TEE enclave** — there is no
> in-process key path. Any node that executes blocks containing offers (a full node
> or a validator) needs a healthy enclave holding the offer key, installed with
> `outbe-cli tee join`. Without it an `offerTribute` tx reverts, and a node that
> reverts an offer the network accepted would diverge on the state root and could
> not follow the chain. (Dispensable only on a chain that carries no offers.) DKG
> consensus key material is separate and lives in `--consensus.keys-dir`, not in the
> enclave.

> **Verified vs documented.** The validator path below — register → follow → stake →
> confirm-ready → reshare → ACTIVE, restart-recovery, and exit — is exercised
> end-to-end by `scripts/e2e/` on a gramine-**mock** localnet (no real SGX/MRENCLAVE
> attestation). The bare `--validator`-off full node is supported by the binary but
> is not covered by those tests.

---

## 0. Prerequisites

```sh
cargo build --release -p outbe-chain  --bin outbe-chain    # the node
cargo build --release -p outbe-cli    --bin outbe-cli      # operator CLI
cargo build --release -p outbe-keygen --bin outbe-keygen   # key generation
```

- The genesis bundle: `genesis.json`, the reth bootnode list, and — for a validator
  (it joins the consensus mesh) — the network's **public** DKG artifacts
  `polynomial.hex` and `dkg-output.hex` (public, no secret share).
- The `outbe-tee-enclave` sidecar (real SGX under gramine in production; a mock
  binary on localnet). Required to execute tribute offers.
- An EVM account (secp256k1) funded with native COEN.

`outbe-cli` / `outbe-keygen` never send key material to the RPC; only signed
transactions and public keys go over the wire.

---

## 1. Full node (sync + RPC only)

Run the node **without** `--validator`. On an offer-bearing chain it still needs an
enclave with the offer key:

```sh
# install the offer key into your enclave (needs only a funded EOA, not a validator):
outbe-cli tee join --enclave-socket 127.0.0.1:7000 \
  --private-key "$EVM_KEY" --rpc-url http://<rpc>:8545 --timeout-secs 60

outbe-chain node \
  --chain /path/to/genesis.json --datadir /var/lib/outbe \
  --bootnodes "<enode URLs>" \
  --http --http.addr 127.0.0.1 --http.port 8545 --http.api eth,net,web3,outbe \
  --tee-enclave-socket 127.0.0.1:7000
```

> **RPC exposure.** Examples bind RPC to `127.0.0.1` and enable only the
> `eth,net,web3,outbe` modules. Never add `admin` or `debug` to `--http.api` on a
> public (`0.0.0.0`) binding: that exposes unauthenticated node control. To serve
> RPC off-host, put it behind authentication or a firewall that restricts access
> to trusted operators, or keep `admin`/`debug` on a local IPC socket. The
> `--consensus.listen-addr 0.0.0.0:30400` P2P port below is the consensus gossip
> listener and is meant to be reachable by peers.

Check it with `outbe-cli monitor health` / `cast block finalized`. A full node’s
`outbe_consensusStatus` reports zeros — those fields are validator-only.

If your goal is to become a validator, skip this and go to section 2 — a validator
runs `node --validator` from the start.

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

### 2.2 Register, announce P2P, install the offer key (one-time)

```sh
# register the validator (binds your address to your BLS pubkey) -> REGISTERED
SIG=$(outbe-keygen sign-registration --key /var/lib/outbe/keys/signing-key.hex \
        --validator-address "$VALIDATOR_ADDR" | grep -oE '[0-9a-f]{120,}' | head -1)
outbe-cli validator register --pubkey "0x$BLS_PUBKEY" --bls-sig "0x$SIG" \
  --private-key "$EVM_KEY" --rpc-url http://<rpc>:8545

# publish your consensus P2P address (the mesh reads it from chain state)
outbe-cli validator set-p2p --symmetric <public-host>:30400 \
  --private-key "$EVM_KEY" --rpc-url http://<rpc>:8545

# start the enclave sidecar, then install the offer key (run BEFORE the node)
outbe-cli tee join --enclave-socket 127.0.0.1:7000 \
  --private-key "$EVM_KEY" --rpc-url http://<rpc>:8545 --timeout-secs 60
```

Registration admits your node to the consensus mesh as a non-voting peer; it does
not by itself make you a validator (no stake, no share yet).

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
  --tee-enclave-socket          127.0.0.1:7000
```

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

A PENDING validator is not admitted to the next reshare until it confirms, on-chain,
that it has caught up — preventing a behind node from being frozen into the committee
before it can vote. Send it **only after** your node is at the finalized tip:

```sh
outbe-cli validator confirm-ready --private-key "$EVM_KEY" --rpc-url http://<rpc>:8545
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
  **resumes signing without a new reshare** — fast, no waiting. (If it had been
  excluded from the set while down, it recovers as a follower and rejoins at the next
  reshare it is part of.)

In both cases, if the node is still waiting on a DKG reshare (you just registered, or
a restarted node missed the ceremony it needed), the normal action is simply to **wait
for the next periodic reshare** — it re-reads the active + confirmed set each epoch and
picks you up. There is no normal on-demand reshare request; the only way to force one
is the disaster-recovery flag `--testnet.force-dkg` (testnet/devnet only, for when all
validators have lost key material — rejected on mainnet).

---

## 4. Validator statuses

`validatorByAddress(addr)` on ValidatorSet (`0x…EE00`) returns the status code:

| Code | Status | Meaning |
|---|---|---|
| 0 | REGISTERED | registered (+ usually P2P-announced + enclave-joined); not staked; non-voting follower |
| 1 | PENDING | staked, awaiting confirm-ready + the reshare that grants a share (excluded from `activeValidatorCount`) |
| 2 | ACTIVE | holds a share; voting |
| 3 | EXITING | left the active set; still accountable (keeps signing) until the next reshare excludes it |
| 4 | UNBONDING | excluded by a reshare; share cleared; stake unbonding |
| 5 | INACTIVE | unbonding complete; stake withdrawn |
| 6 | JAILED | punished on a felony (slashed + frozen); dropped from the committee at the next reshare, but kept in the registry pending unjail or unstake |

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
which excludes you (EXITING → **UNBONDING**, share cleared). Your node then **falls
back to the share-less follower phase** of the smaller committee — it stays online
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

For the enclave: with sealing (`--tee-dir <path>` + `--chain-id`) the sidecar restores
the **same** offer key across restarts — no fresh `tee join`. Without sealing (no
`--tee-dir`, e.g. the gramine-direct mock localnet) a sidecar restart re-derives a
**new** offer key and needs a fresh `tee join`; keep the sidecar running across a node
restart in that case.

---

## 7. Reference

### Protocol addresses

| Precompile | Address |
|---|---|
| ValidatorSet | `0x000000000000000000000000000000000000EE00` |
| Staking | `0x000000000000000000000000000000000000EE02` |
| TeeRegistry | `0x000000000000000000000000000000000000EE0A` |

### Key node flags

| Flag | Purpose |
|---|---|
| `--validator` | run the consensus thread (validator); omit for a full node (EL sync + RPC only) |
| `--consensus.signing-key` / `--validator.evm-key` | BLS signing key / secp256k1 system-tx signer (validator) |
| `--consensus.signing-share` | BLS threshold share — present only once the node holds a share |
| `--consensus.public-polynomial` / `--consensus.dkg-output` | public DKG artifacts to follow finality before holding a share |
| `--consensus.keys-dir` | where the DKG share/polynomial/output are persisted (default `<datadir>/keys`) |
| `--consensus.listen-addr` / `--consensus.peers` | consensus P2P listen address / bootstrap hint `<bls_pubkey>@<host:port>` |
| `--tee-enclave-socket` | enclave sidecar socket (needed to execute tribute offers); the node fail-fasts without a healthy attested enclave |
| `--testnet.force-dkg` | disaster-recovery only: force a fresh DKG when all validators lost key material (testnet/devnet, rejected on mainnet) |

### Operator commands

| Command | Purpose |
|---|---|
| `outbe-keygen hybrid` / `show-pubkey` / `sign-registration` | generate keys / derive BLS pubkey / sign registration |
| `outbe-cli tee join` | register the enclave + install the offer key (funded EOA only) |
| `outbe-cli validator register` / `set-p2p` | register (→ REGISTERED) / publish the P2P address |
| `outbe-cli staking stake` / `unstake` / `claim` | stake (→ PENDING at `min_stake`) / unstake / withdraw |
| `outbe-cli staking unjail` | return a JAILED validator → PENDING (stake ≥ min_stake) |
| `outbe-cli validator confirm-ready` | confirm caught-up (stale-join guard) |
| `outbe-cli validator deactivate` | leave the active set (→ EXITING) |
| `outbe-cli monitor health` / `readiness` / `watch` | health / readiness / dashboard |
| `outbe-cli validator participation` / `list` / `info` | participation + set inspection |

---

## Localnet quickstart

The validator path end-to-end (a 4-validator gramine-mock localnet) is in
`scripts/e2e/`:

```sh
cargo build -p outbe-chain --bin outbe-chain
cargo build --release -p outbe-tee-enclave --features mock --bin outbe-tee-enclave-mock
sudo true   # the scripts use sudo for run-testnet.sh / docker
scripts/e2e/s1_s2_s6_s3_lifecycle.sh   # follow → stake/confirm → ACTIVE → exit
```

`scripts/e2e/lib.sh` holds the exact localnet command lines this guide generalizes;
see `scripts/e2e/README.md`.
