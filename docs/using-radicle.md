# Working with Radicle on an Outbe network

Every validator runs an `outbe-radicle` sidecar — a Heartwood node whose peer
set and repository set come from chain state rather than a local config file.
That single difference governs everything here.

A stock Heartwood node seeds whatever its operator tells it to. An Outbe
sidecar seeds **only the repository ids recorded in the `RadicleRegistry`
precompile** (`0x…EE11`), reconciled on every finalized block
(`crates/blockchain/radicle/src/manager/actor.rs:465`). Pushing at a validator
does nothing until the repository id is registered on-chain, and `rad` cannot
do that registration — it is an EVM transaction.

So there are two layers, and they are managed by different tools:

| Layer | Owner | Tool |
|---|---|---|
| Which repositories the network holds | the chain | `outbe-cli radicle` |
| Client config, node lifecycle | your machine | `outbe-cli rad` |
| Code, issues, patches, delegates | the repository | `rad`, `git` |

---

## 1. Set up your client

You need `rad` and `radicle-node` on PATH (Radicle 1.9.x) and `outbe-cli`
built from this repository.

`rad auth` creates your identity keys — and that is all it does for you here.
The config it writes points at the public Radicle seeds and accepts every
repository it is offered; neither is right for an Outbe network. The identity
itself is network-agnostic: the same key works anywhere, so `rad auth` does
not put you "on our network". **The config does.**

```bash
export RAD_HOME=/tmp/radt          # keep this path SHORT — see Troubleshooting
rad auth --alias my-laptop

outbe-cli --rpc-url http://125.253.90.171/ rad init  --home /tmp/radt
outbe-cli --rpc-url http://125.253.90.171/ rad start --home /tmp/radt
```

`rad init` reads the validator set, each validator's Radicle NodeId binding
and its P2P host **from chain state** — nothing is hard-coded — and rewrites
only the network-facing settings, leaving your keys and unrelated preferences
alone:

```
Peers (4):
  0x97Cf63ACd02BE0d6Da11FE5C9b834167776a5a50  z6Mkkmc…LnSNDKE@125.253.90.171:8776
  0xb49F08B2819726005d6E3b84074F627E7011401B  z6MkvhQ…2D51G7a@131.153.159.3:8776
  0xBf663D6C0f5dA824Fe7857a78E58b4E53Ec53af5  z6Mkgqj…4T922qo@125.253.92.5:8776
  0x306f3c20c1c78E9C977A21072a7BEDe063F7d387  z6MksyT…tZFdM5x@192.240.203.51:8776
```

Four settings do the work, and each closes its own hole:

| Setting | Why it matters |
|---|---|
| `preferredSeeds: []` | The one reason a freshly authed node reaches the public network. Top-level, not under `node` — easy to miss. |
| `peers: {type: static}` + `connect` | Talk to exactly this list; no dynamic discovery. |
| `seedingPolicy: {default: block}` | Otherwise the node accepts every announcement it hears — 51 foreign repositories and 197 MB in a couple of hours. |
| `relay: auto` | Validators run `relay: always` because they are reachable seeds with external addresses. On a loopback client that value stops sessions establishing at all. |

Confirm you are connected to the validators and nobody else:

```bash
rad node status
```

```
z6Mkkmc…LnSNDKE   125.253.90.171:8776   ✓   ↗   9s
z6MkvhQ…2D51G7a   131.153.159.3:8776    ✓   ↗   9s
z6Mkgqj…4T922qo   125.253.92.5:8776     ✓   ↗   9s
z6Mksy…oYtZFdM5x  192.240.203.51:8776   ✓   ↗   9s
```

Port `8776` must be reachable from wherever you run `rad`. The launch bundle's
firewall admits only the peer validators; opening it to clients is a
deliberate decision (`sudo ufw allow 8776/tcp` on each validator).

### Node lifecycle

```bash
outbe-cli rad status  --home /tmp/radt    # local state + drift from chain state
outbe-cli rad restart --home /tmp/radt
outbe-cli rad stop    --home /tmp/radt
```

`rad start` puts the node in its own process group, so it survives the shell
that started it. `--home` may be omitted; it falls back to `$RAD_HOME`, then
`~/.radicle`.

`rad status` treats the chain as the source of truth and reports drift rather
than the config's own account of itself — a peer the chain knows but the
config lacks shows as `MISSING`, one the config has but the chain does not as
`EXTRA`. Re-run `rad init` after any validator set change.

---

## 2. Publish a repository

Two steps: create it locally, then register its id on-chain. Skipping the
second means no validator will ever hold it.

```bash
cd my-project
git init && git commit --allow-empty -m "initial commit"
rad init --name my-project --description "…" \
         --default-branch "$(git branch --show-current)" --no-confirm --public
rad .            # prints rad:z4Advr3ie8rvVHN8rXaBdvunobKas
```

Pass `--public` or `--private` explicitly — `--no-confirm` does not answer the
visibility prompt, and without one of them a non-interactive shell dies with
`The input device is not a TTY`.

The RID is a 20-byte identifier in base58btc multibase. `RepoId` is
`FixedBytes<20>` and the CLI rejects anything else, so decode it rather than
padding it to 32 bytes:

```bash
python3 - "$(rad .)" <<'PY'
import sys
A = '123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz'
s = sys.argv[1].removeprefix("rad:")
assert s.startswith("z"), "expected a base58btc multibase RID"
s = s[1:]
n = 0
for c in s:
    n = n * 58 + A.index(c)
body = n.to_bytes((n.bit_length() + 7) // 8 or 1, "big")
body = b"\x00" * (len(s) - len(s.lstrip("1"))) + body   # leading '1' == zero byte
assert len(body) == 20, "RepoId must be 20 bytes, got %d" % len(body)
print("0x" + body.hex())
PY
```

Register it. Any funded account may register; the caller is recorded as the
registrant. **The registry is append-only — a repository id cannot be removed
once written.**

```bash
outbe-cli --rpc-url http://125.253.90.171/ --private-key <hex> \
    radicle register-repository --repo-id 0x<20-byte-hex>
```

Within seconds of the receipt every validator picks it up. Worked example on
chain `54322345`:

```
rad:z4Advr3ie8rvVHN8rXaBdvunobKas
  -> RepoId  0xe3433974a88a53c89cf4788105485439fb45b104
  -> block 2245, registrant 0x97Cf63ACd02BE0d6Da11FE5C9b834167776a5a50
```

---

## 3. Connect to an existing repository

Anyone whose client is configured as in step 1 can clone a registered
repository. No permission is needed to read.

```bash
rad clone rad:z4Advr3ie8rvVHN8rXaBdvunobKas
cd outbe-demo
```

The clone brings the code **and** the collaborative objects — issues and
patches arrive with it:

```
│ outbe-demo                                    │
│ First repository in the Outbe RadicleRegistry │
│ 1 issues · 1 patches                          │
```

If you already have the repository in local storage (the node refetches on
startup), `rad clone` just lays down the working copy.

To point an existing git checkout at a Radicle repository instead, use
`rad clone` and move your work across — the `rad` remote is set up by `rad
init` / `rad clone`, not by hand.

---

## 4. Everyday work

### Pushing and pulling

```bash
git push              # publish your branch, then announce it
git pull              # from the canonical upstream
rad sync --fetch      # pull refs from the network without merging
rad sync --announce   # re-announce your refs
```

**Every peer writes to its own namespace.** A push always succeeds — including
from someone who is not a delegate — but it updates *your* branch under *your*
NodeId, not the repository's canonical head:

```
To rad://z4Advr3ie…/z6Mkt2F5arX2Rf1a2H8jBUJo5D7ERzU637JK13P9EEaWStsk
 * [new branch]      main -> main
```

The canonical head moves only when a delegate pushes. This is why a push
succeeding is not evidence that you have write access.

### Seeing someone else's work

Add their NodeId as a remote:

```bash
rad remote add z6Mkt2F5arX2Rf1a2H8jBUJo5D7ERzU637JK13P9EEaWStsk \
    --name teammate --fetch
git log --oneline teammate/main
```

```bash
rad remote list       # who you track, and which one is canonical upstream
```

### Issues

```bash
rad issue open --title "First issue" --description "…"
rad issue list
rad issue show <id>
```

### Patches

A patch is a push to `refs/patches`:

```bash
git checkout -b feature/my-change
git commit -am "…"
git push -o patch.message="Add a demo section" rad HEAD:refs/patches
```

```
✓ Patch 42c3f1d56c06c969c383abd680763b5be9b111cb opened
```

```bash
rad patch list
rad patch show <id>
git push rad --force-with-lease HEAD:patches/<id>   # update an open patch
```

Issues and patches replicate through the validators like code does.

---

## 5. Granting write access

Delegates are the accounts whose pushes move the canonical head. Add one by
DID (their NodeId with a `did:key:` prefix):

```bash
rad id update --title "Add teammate as delegate" \
              --description "Grant write access" \
              --delegate did:key:z6Mkt2F5arX2Rf1a2H8jBUJo5D7ERzU637JK13P9EEaWStsk
rad sync --announce
```

```bash
rad inspect --delegates
```

```
did:key:z6MkvAA84kiFuc5gL4z7STMNdFLa9Ebt4zKA3AwJGQissXZW (my-laptop)
did:key:z6Mkt2F5arX2Rf1a2H8jBUJo5D7ERzU637JK13P9EEaWStsk (teammate)
```

The `rad sync --announce` is not optional. Without it the change sits in your
local storage: the new delegate runs `rad sync --fetch` and still sees the old
single-delegate identity. Announce, then have them fetch.

`--threshold` sets how many delegate signatures an identity change needs; it
stays at 1 unless you raise it.

---

## 6. Checking state

Ask the chain what it holds:

```bash
outbe-cli --rpc-url http://125.253.90.171/ radicle repositories
outbe-cli --rpc-url http://125.253.90.171/ radicle repository --repo-id 0x<20-byte-hex>
```

Ask a sidecar what it is actually seeding. `desiredRepositoryCount` comes from
chain state; `availableRepositoryCount` is how many it has fetched — they
converge when replication finishes:

```bash
curl -s -X POST http://125.253.90.171/ -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"outbe_radicleStatus","params":[]}' | python3 -m json.tool
```

```json
{
  "phase": "ready",
  "desiredRepositoryCount": 1,
  "availableRepositoryCount": 1,
  "pendingRepositoryCount": 0,
  "connectedPeerCount": 3
}
```

From the client side, the validators appear under the alias `outbe` and should
all report the same `SigRefs`:

```bash
rad sync status
```

```
│ z6MkvhQ…2D51G7a   outbe   ✓   3a81980   5 seconds ago │
│ z6Mkgqj…4T922qo   outbe   ✓   3a81980   5 seconds ago │
│ z6Mkkmc…LnSNDKE   outbe   ✓   3a81980   5 seconds ago │
│ z6MksyT…tZFdM5x   outbe   ✓   3a81980   7 seconds ago │
```

`rad sync status` listing only public Radicle seeds and none of the validator
NodeIds means the repository id is not registered on-chain. That is the single
most common cause of "it doesn't work".

---

## 7. Recovering a repository

Once registered, the validators are a complete replica — a lost working copy
and a lost local storage are both recoverable.

Back up `$RAD_HOME/keys` first. Those keys are your identity, not repository
data: delete them and you can still clone and read, but you lose delegate
rights and can never push to that repository again.

```bash
cp -a "$RAD_HOME/keys" ~/rad-keys-backup
outbe-cli rad stop --home "$RAD_HOME"
rm -rf "$RAD_HOME/storage" "$RAD_HOME/cobs" ./my-project

outbe-cli rad start --home "$RAD_HOME"
rad clone rad:z4Advr3ie8rvVHN8rXaBdvunobKas
```

The node refetches on startup, so the repository is usually back in storage
before `rad clone` runs. Confirm which peer served it — the log names the
source, and it should be a validator NodeId, not a public seed:

```bash
grep Fetched "$RAD_HOME/node/outbe-rad.log"
# Fetched rad:z4Advr3ie… from z6Mkgqj256ktQWXb4XnN1EeDeQQkCjftpXBt8QDtA4T922qo successfully
```

Then verify the recovered tree matches what was pushed:

```bash
cd outbe-demo && git log --oneline && git rev-parse HEAD
```

---

## 8. Working on a validator host

Each sidecar keeps a standard Heartwood home, so `rad` can drive it directly
on the validator. One difference: the control socket is named
`outbe-control.sock`, not Heartwood's default.

```bash
export RAD_HOME=/opt/outbe-chain/keys/validator-0/radicle
export RADICLE_CONTROL_SOCKET=$RAD_HOME/node/outbe-control.sock
rad node status
rad ls
```

Do not `rad node start` / `rad node stop` there, and do not run `outbe-cli rad
init` against that home. The sidecar owns that process and reconciles its peer
set from chain state; a manually started node competes with
`outbe-radicle@0.service` for the socket.

---

## Command reference

| Command | What it does |
|---|---|
| `outbe-cli rad init` | Write the client config from chain state |
| `outbe-cli rad start` / `stop` / `restart` | Node lifecycle |
| `outbe-cli rad status` | Local state and drift from chain state |
| `outbe-cli radicle register-repository` | Register a RepoId on-chain (append-only) |
| `outbe-cli radicle repositories` | List registered RepoIds |
| `outbe-cli radicle repository --repo-id` | Who registered a RepoId |
| `rad auth` | Create identity keys (once) |
| `rad init` / `rad clone` | Create or fetch a repository |
| `rad ls`, `rad .`, `rad inspect` | What you hold, this repo's RID, its identity |
| `rad seed` / `rad unseed` | Seeding policy per repository |
| `rad sync --fetch` / `--announce` / `status` | Replication |
| `rad remote add` / `list` | Track another peer's refs |
| `rad issue`, `rad patch` | Collaborative objects |
| `rad id update --delegate` | Grant or revoke write access |

## Troubleshooting

| Symptom | Cause |
|---|---|
| `path must be shorter than SUN_LEN` | `RAD_HOME` too long. Unix sockets cap the path near 104 bytes — use `/tmp/radt`, not a deep nested directory. |
| `rad node status` shows public seeds | `preferredSeeds` still holds them, or `peers` is `dynamic`. Run `outbe-cli rad init`. |
| Peers listed but never connect (`✗`) | `relay: always` on a client. `rad init` sets it to `auto`. |
| The node dies when the shell closes | Started by hand instead of `outbe-cli rad start`, which detaches it into its own process group. |
| Validators connect but never hold the repo | The repository id is not in `RadicleRegistry`. `rad node connect` creates a session but does not make a sidecar seed anything. |
| Push succeeds but the head does not move | You are not a delegate. The push landed in your own namespace. |
| A new delegate still sees the old identity | The owner did not `rad sync --announce` after `rad id update`. |
| `reference 'refs/heads/master' not found` | `--default-branch` names a branch the repository does not have. |
| `The input device is not a TTY` | `rad init` without `--public` or `--private`. |
| `RepoId must be exactly 20 bytes` | The RID was padded to 32. Decode the multibase value instead. |
| `desiredRepositoryCount` stays 0 | The registry is empty, or the sidecar has not seen a finalized block carrying the registration. |
| Connection refused on 8776 | Firewall. The launch bundle admits only peer validators by default. |
| Storage fills with unknown repositories | `seedingPolicy` is `allow`. Set it to `block` via `rad init`, `rad unseed` the strays, and delete their directories under `$RAD_HOME/storage` — `rad` has no command to remove a repository from storage. |

## Limits worth knowing

`network: "outbe"` cannot be set on a stock client: `radicle-node` 1.9.1
accepts only `main` or `test` and refuses to start otherwise. The validators
run a fork (`outbe/outbe-heartwood`, `Cargo.toml:137`) that understands it.

A stock client on `network: main` nonetheless connects to those validators,
clones and pushes — so **the network id does not isolate the network at the
handshake**. Isolation here is client-side configuration only. Anyone who
knows a NodeId and address can connect with stock `rad`, and `8776` is open.
If the network is meant to be private, that needs solving somewhere other than
the client config.
