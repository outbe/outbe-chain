# Using the `rad` CLI against an Outbe network

Every validator runs an `outbe-radicle` sidecar: a Heartwood node whose peer
set and repository set are driven by chain state, not by a local config file.
This is the difference that governs everything below.

A stock Heartwood node seeds whatever its operator tells it to. An Outbe
sidecar seeds **only the repository ids recorded in the `RadicleRegistry`
precompile** (`0x…EE11`), reconciled on every finalized block
(`crates/blockchain/radicle/src/manager/actor.rs:465`). Pushing a repository
at a validator does nothing until the repository id is registered on-chain.
`rad` cannot do that registration — it is an EVM transaction.

So the flow has three parts: create the repository locally, register its id
on-chain, then confirm the validators picked it up.

## 1. Point a local `rad` at the network

`rad auth` creates the identity keys, and that is all it does for you here —
the config it writes points at the public Radicle seeds and accepts every
repository it is offered. Neither is right for an Outbe network. The identity
itself is network-agnostic: the same key works anywhere, so `rad auth` does
not put you "on our network", the config does.

```bash
export RAD_HOME=/tmp/radt          # keep this path SHORT — see Troubleshooting
rad auth --alias my-laptop         # creates the keys; leaves you on public seeds

outbe-cli --rpc-url http://125.253.90.171/ rad init --home /tmp/radt
outbe-cli --rpc-url http://125.253.90.171/ rad start --home /tmp/radt
```

`rad init` reads the validator set, each validator's Radicle NodeId binding
and its P2P host from chain state — nothing is hard-coded — and rewrites only
the network-facing settings, leaving your keys and unrelated preferences
alone. It prints what it wired up:

```
Peers (4):
  0x97Cf63ACd02BE0d6Da11FE5C9b834167776a5a50  z6Mkkmc…LnSNDKE@125.253.90.171:8776
  0xb49F08B2819726005d6E3b84074F627E7011401B  z6MkvhQ…2D51G7a@131.153.159.3:8776
  0xBf663D6C0f5dA824Fe7857a78E58b4E53Ec53af5  z6Mkgqj…4T922qo@125.253.92.5:8776
  0x306f3c20c1c78E9C977A21072a7BEDe063F7d387  z6MksyT…tZFdM5x@192.240.203.51:8776
```

Four settings do the work, and each closes its own hole:

| Setting | Why |
|---|---|
| `preferredSeeds: []` | The one reason a freshly authed node reaches the public network. Top-level, not under `node`, so it is easy to miss. |
| `peers: {type: static}` + `connect` | Talk to exactly this list; no dynamic discovery. |
| `seedingPolicy: {default: block}` | Otherwise the node accepts every announcement — 51 foreign repositories and 197 MB in a couple of hours. |
| `relay: auto` | Validators run `relay: always` because they are reachable seeds. On a loopback client that value stops sessions establishing at all. |

The rest of the lifecycle:

```bash
outbe-cli rad status --home /tmp/radt     # local state, and drift from chain state
outbe-cli rad restart --home /tmp/radt
outbe-cli rad stop --home /tmp/radt
```

`rad status` treats the chain as the source of truth and reports drift rather
than the config's own account of itself — a peer that the chain knows but the
config lacks shows as `MISSING`, one the config has but the chain does not as
`EXTRA`. Re-run `rad init` after a validator set change.

`--home` may be omitted; it falls back to `$RAD_HOME`, then `~/.radicle`.

`rad node status` should then list all four validators. A `✓` in the
connection column means a live session:

```
z6Mkkmc…LnSNDKE   125.253.90.171:8776   ✓   ↗   9s
z6Mkvh…2D51G7a    131.153.159.3:8776    ✓   ↗   9s
z6Mkgqj…A4T922qo  125.253.92.5:8776     ✓   ↗   9s
z6Mksy…oYtZFdM5x  192.240.203.51:8776   ✓   ↗   9s
```

Port 8776 must be reachable from wherever you run `rad`. The launch bundle's
firewall rules open it to the peer validators only; opening it more widely is
a deliberate decision:

```bash
sudo ufw allow 8776/tcp        # on each validator, to admit external clients
```

## 2. Create a repository and register its id

```bash
cd my-project
git init && git commit --allow-empty -m "initial commit"
rad init --name my-project --description "…" \
         --default-branch "$(git branch --show-current)" --no-confirm --public
rad .            # prints rad:z2hJcK6o29Sx7LUiGAX8UpcjPUDGN
```

Pass `--public` or `--private` explicitly. `--no-confirm` does not cover the
visibility prompt, so without one of them a non-interactive shell dies with
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

Register it. Any funded account may register — the caller is recorded as the
registrant. The registry is **append-only**: a repository id cannot be
removed once written.

```bash
outbe-cli --rpc-url http://125.253.90.171/ --private-key <hex> \
    radicle register-repository --repo-id 0x<20-byte-hex>
```

Registration takes effect for the sidecars once the block is finalized —
in practice within a few seconds of the receipt.

Worked example, registered on chain `54322345`:

```
rad:z4Advr3ie8rvVHN8rXaBdvunobKas
  -> RepoId  0xe3433974a88a53c89cf4788105485439fb45b104
  -> block 2245, registrant 0x97Cf63ACd02BE0d6Da11FE5C9b834167776a5a50
```

## 3. Confirm the network picked it up

Ask the chain what it holds:

```bash
outbe-cli --rpc-url http://125.253.90.171/ radicle repositories
outbe-cli --rpc-url http://125.253.90.171/ radicle repository --repo-id 0x<20-byte-hex>
```

Ask a sidecar what it is actually seeding. `desiredRepositoryCount` is the
count from chain state; `availableRepositoryCount` is how many of those it
has fetched. They converge when replication finishes:

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

And from the client side:

```bash
rad sync status                 # which nodes hold your repo
rad sync --announce             # re-announce after new commits
git push                        # publishes to the seeds that hold it
```

The validators appear under the alias `outbe`, and all four should report the
same `SigRefs` once replication settles:

```
│ z6MkvhQ…2D51G7a   outbe   ✓   3a81980   5 seconds ago │
│ z6Mkgqj…4T922qo   outbe   ✓   3a81980   5 seconds ago │
│ z6Mkkmc…LnSNDKE   outbe   ✓   3a81980   5 seconds ago │
│ z6MksyT…tZFdM5x   outbe   ✓   3a81980   7 seconds ago │
```

`rad sync status` listing only public Radicle seeds and none of the four
validator NodeIds means the repository id is not registered on-chain — go
back to step 2. It is the single most common cause.

After that, `git push` replicates normally: each new commit reaches all four
validators within seconds and `SigRefs` advances in step across them.

## Recovering a repository from the network

Once registered, the validators are a complete replica — a lost working copy
and a lost local storage are both recoverable from them.

Back up `$RAD_HOME/keys` first. Those keys are your identity, not repository
data: delete them and you can still clone and read the repository, but you
lose delegate rights and can never push to it again.

```bash
cp -a "$RAD_HOME/keys" ~/rad-keys-backup
pkill -f radicle-node
rm -rf "$RAD_HOME/storage" "$RAD_HOME/cobs" ./my-project

radicle-node --listen 127.0.0.1:8790 &
rad clone rad:z4Advr3ie8rvVHN8rXaBdvunobKas
```

The node refetches on startup, so the repository is usually back in storage
before `rad clone` even runs. Confirm which peer served it — the log names
the source, and it should be a validator NodeId, not a public seed:

```bash
grep Fetched /path/to/node.log
# Fetched rad:z4Advr3ie… from z6Mkgqj256ktQWXb4XnN1EeDeQQkCjftpXBt8QDtA4T922qo successfully
```

Then verify the recovered tree matches what was pushed:

```bash
cd outbe-demo && git log --oneline && git rev-parse HEAD
```

## Talking to a validator's own Radicle home

Each sidecar keeps a standard Heartwood home, so `rad` can drive it directly
on the validator host. One difference: the control socket is named
`outbe-control.sock`, not Heartwood's default.

```bash
export RAD_HOME=/opt/outbe-chain/keys/validator-0/radicle
export RADICLE_CONTROL_SOCKET=$RAD_HOME/node/outbe-control.sock
rad node status
rad ls
```

Do not `rad node start` or `rad node stop` there. The sidecar owns that
process, and its peer set is reconciled from chain state — a manually started
node competes with `outbe-radicle@0.service` for the socket.

## Troubleshooting

| Symptom | Cause |
|---|---|
| `path must be shorter than SUN_LEN` | `RAD_HOME` too long. Unix sockets cap the path near 104 bytes — use `/tmp/radt`, not a deep nested directory. |
| `rad node status` shows only public seeds | `preferredSeeds` still holds them, or `peers` is `dynamic`. Run `outbe-cli rad init`. |
| Peers listed but never connect (`✗`) | `relay: always` on a client. `rad init` sets it to `auto`. |
| The node dies when the shell closes | Started by hand instead of `outbe-cli rad start`, which puts it in its own process group. |
| Validators connect but never hold the repo | The repository id is not in `RadicleRegistry`. `rad node connect` creates a session but does not make a sidecar seed anything. |
| `reference 'refs/heads/master' not found` | `--default-branch` names a branch the repository does not have. |
| `The input device is not a TTY` | `rad init` without `--public` or `--private`; `--no-confirm` does not answer the visibility prompt. |
| `RepoId must be exactly 20 bytes` | The RID was padded to 32. Decode the multibase value instead. |
| `desiredRepositoryCount` stays 0 | The registry is empty, or the sidecar has not yet seen a finalized block carrying the registration. |
| Connection refused on 8776 | Firewall. The launch bundle admits only peer validators by default. |
