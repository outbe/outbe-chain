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

`rad auth` writes a config whose peer discovery is dynamic, which sends your
node to the public Radicle seeds instead of ours. Overwrite the peer settings
before starting the node:

```bash
export RAD_HOME=/tmp/radt          # keep this path SHORT — see Troubleshooting
rad auth --alias my-laptop
```

Collect each validator's Heartwood NodeId from its RPC endpoint. The chain
stores it as 32 raw bytes; Heartwood addresses it as a multibase `z…` string,
so it needs converting:

```bash
python3 - <<'PY'
import json, urllib.request
IPS = ["125.253.90.171", "131.153.159.3", "125.253.92.5", "192.240.203.51"]
A = '123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz'

def nid(ip):
    req = urllib.request.Request("http://%s/" % ip,
        data=json.dumps({"jsonrpc":"2.0","id":1,
                         "method":"outbe_radicleStatus","params":[]}).encode(),
        headers={"content-type":"application/json"})
    h = json.load(urllib.request.urlopen(req, timeout=10))["result"]["localNodeId"]
    n = int.from_bytes(b'\xed\x01' + bytes.fromhex(h.removeprefix("0x")), "big")
    s = ""
    while n:
        n, r = divmod(n, 58); s = A[r] + s
    return "z" + s

cfg = json.load(open("/tmp/radt/config.json"))
cfg["node"]["connect"] = ["%s@%s:8776" % (nid(ip), ip) for ip in IPS]
cfg["node"]["peers"] = {"type": "static"}
cfg["node"]["seedingPolicy"] = {"default": "allow", "scope": "all"}
json.dump(cfg, open("/tmp/radt/config.json", "w"), indent=2)
for c in cfg["node"]["connect"]:
    print(" ", c)
PY

radicle-node --listen 127.0.0.1:8790 &
rad node status
```

`rad node status` should list all four validators. A `✓` in the connection
column means a live session:

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
| `rad node status` shows only public seeds | `connect` is empty and `peers` is `dynamic`; the node used its built-in bootstrap list. Rewrite the config as in step 1 and restart. |
| Validators connect but never hold the repo | The repository id is not in `RadicleRegistry`. `rad node connect` creates a session but does not make a sidecar seed anything. |
| `reference 'refs/heads/master' not found` | `--default-branch` names a branch the repository does not have. |
| `The input device is not a TTY` | `rad init` without `--public` or `--private`; `--no-confirm` does not answer the visibility prompt. |
| `RepoId must be exactly 20 bytes` | The RID was padded to 32. Decode the multibase value instead. |
| `desiredRepositoryCount` stays 0 | The registry is empty, or the sidecar has not yet seen a finalized block carrying the registration. |
| Connection refused on 8776 | Firewall. The launch bundle admits only peer validators by default. |
