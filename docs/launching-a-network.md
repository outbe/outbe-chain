# Launching an Outbe network

One yaml describes the network; one command turns it into a genesis and a
per-machine archive you copy to each validator. This page is the whole
procedure, including the failures that are easy to hit and hard to read.

## What you need first

**On each validator machine:** SGX hardware (`/dev/sgx_enclave`,
`/dev/sgx_provision`), a running `aesmd.service`, Gramine, Docker (for
MongoDB), and the `outbe-chain`, `outbe-ocomp`, `outbe-radicle`,
`outbe-feeder` binaries. `docs/launching-with-sgx.md` lists the exact SGX
package set.

**On one build machine:** the same binaries plus the Gramine signing key at
`~/.config/gramine/enclave-key.pem`, used to sign the enclave once.

**Key material** for every founder, produced with:

```bash
outbe-keygen validator --output-dir keys/validator-0 --chain-id <id>
```

That gives `signing-key.hex` (BLS consensus), `evm-key.hex`, the stable
`reth-p2p-secret.hex` transport identity, the dedicated
`ocomp-evm-key.hex` operational signer, and the Radicle identity. The command
prints the OCOMP signer address and the `validator delegate ocomp` command that
the validator must submit after registration. The OCOMP result-signing
registration and `ocomp-key-v1.hex` are *not* generated here — they bind the
genesis hash, which does not exist yet, so `create_genesis.py` mints them during
the run.

## Sign the enclave once

Do this on the build machine, not on each validator. Signing per machine gives
every host its own `mr_signer` — four different enclave identities on one
network — and a `dcap-required` genesis pins a single `mrsigner`, so three of
four machines would reject it.

```bash
cd /opt/outbe-chain
gramine-manifest \
  -Dlog_level=error -Darch_libdir=/lib/x86_64-linux-gnu \
  -Dentrypoint=/opt/outbe-chain/outbe-tee-enclave \
  -Dtee_dir=/opt/outbe-chain/tee \
  -Dremote_attestation=none \
  outbe-tee-enclave.manifest.template outbe-tee-enclave.manifest
gramine-sgx-sign --key ~/.config/gramine/enclave-key.pem \
  --manifest outbe-tee-enclave.manifest --output outbe-tee-enclave.manifest.sgx
gramine-sgx-sigstruct-view outbe-tee-enclave.sig   # note mr_enclave / mr_signer
```

`remote_attestation=none` is the SGX-without-DCAP profile: a real enclave on
real hardware, no Intel collateral. For an attested network use `dcap`, and
supply the measurements under `tee:` in the yaml.

## Describe the network

```yaml
chain_id: 54322345

# One entry per founding validator, in order. OCOMP V1 fixes the committee at
# four. Use the addresses the machines can actually reach each other on —
# private subnets that do not route between all hosts will not work.
validators:
  - 203.0.113.10
  - 203.0.113.11
  - 203.0.113.12
  - 203.0.113.13

keys_dir: ./keys                 # keys_dir/validator-N/ from outbe-keygen

tee:
  mode: gramine-direct-dev       # explicit real SGX, no Intel collateral
signed_enclave_dir: ./signed     # the artifacts you just signed

# Where the bundle lands on the machines.
remote_base_dir: /opt/outbe-chain
remote_keys_dir: /opt/outbe-chain/keys

node_binary: /opt/outbe-chain/outbe-chain
ocomp_binary: /opt/outbe-chain/outbe-ocomp
radicle_binary: /opt/outbe-chain/outbe-radicle
feeder_binary: /opt/outbe-chain/outbe-feeder
```

Do **not** pin `timestamp:` unless you are reproducing an existing genesis.
The TEE lease runs from the genesis timestamp, so a stamp more than a few
hours old makes block 1 fail with `requested lease is already expired`.
`create_genesis.py` refuses such a genesis; `allow_stale_timestamp: true`
overrides it deliberately.

`scripts/network.example.yaml` lists every parameter with its default;
`scripts/testnet.yaml` is the testnet profile and the source of defaults for
anything a network does not state.

## Build it

```bash
python3 scripts/create_genesis.py network.yaml
```

Output:

```
genesis.json                 seeded state + block-1 OCOMP and TEE manifests
protocol-bundle-v1.ocb1      canonical OCOMP bundle
reth-bootnodes.txt           stable enodes
validator-N/                 run scripts, Caddyfile, preflight
systemd/                     one templated unit per role
enclave/                     the signed enclave (never the private key)
dist/validator-N.tgz         one self-contained archive per machine
dist/SHA256SUMS              checksums for those archives
DEPLOY.md                    the launch plan for this specific network
```

`dist/` is what you ship. Each archive holds that machine's run scripts,
systemd units, the shared genesis and bootnodes, the signed enclave, and
**only that validator's key material**.

## Deploy and start

```bash
# from dist/, for machine N
scp validator-N.tgz SHA256SUMS unpack.sh <machine-N>:
ssh <machine-N> './unpack.sh N'

# on the machine
cd /opt/outbe-chain/validator-N && ./preflight.sh N
sudo /opt/outbe-chain/install-systemd.sh N
./install-caddy.sh          # publishes RPC; the node itself stays on loopback
```

`preflight.sh` prints the genesis and enclave digests and refuses to pass on
leftover state. Compare its digests across all four machines before starting —
a mismatched genesis is the single most common cause of a network that comes
up and then stalls.

Start all founders within a few minutes of each other: block 1 carries the
founding DKG ceremony and needs every genesis validator online.

## Verify

```bash
curl -s -X POST http://<machine>/ -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"eth_blockNumber","params":[]}'

systemctl list-units 'outbe-*' --no-pager     # 6 services per machine
journalctl -u outbe-node@0 -f
```

Healthy means: every machine reports an advancing block number, `net_peerCount`
is `n-1` on each, and all six services are `active`.

## Ports

Between the validators: consensus p2p `30400`, reth p2p `30303` (TCP+UDP),
discv5 `31303` (UDP), Radicle replication `8776`. Admitting an external `rad`
client means opening `8776` more widely — see `docs/using-radicle.md`.

Published by caddy: RPC on `80`, Radicle status on `8080`.

Loopback only, never exposed: RPC `8545`, authrpc `8551`, metrics `9101`,
Radicle status `8876`, the enclave socket `17000`, MongoDB `27017`, and the
node-owned OCOMP endpoints `30401-30406` (HTTP registration, ZeroMQ and Worker
observability for the default consensus port `30400`).

## What runs on each machine

| Component | How it runs | Note |
|---|---|---|
| Execution + consensus | `outbe-chain node` | one binary, no Engine API split |
| TEE enclave | `gramine-sgx`, host process | node will not start without it |
| Radicle sidecar | `outbe-radicle` | validator startup requires its control socket; seeds only repository ids registered on-chain (`docs/using-radicle.md`) |
| OCOMP Supervisor | embedded in `outbe-chain` | ExEx discovers finalized jobs and owns scheduling, vote and payout journals |
| OCOMP SnapshotExporter | `outbe-ocomp snapshot-exporter` | own process |
| OCOMP Worker | `outbe-ocomp worker` | own process |
| Price feeder | `outbe-feeder` | submits oracle votes |
| Projection store | MongoDB container | replica set `rs0`, mandatory |

The six external services run under systemd as
`outbe-<role>@<index>.service`; the Supervisor shares the node service.

## Failures worth recognising

| Symptom | Cause |
|---|---|
| `requested lease is already expired` at block 1 | genesis timestamp too old |
| `projection identity does not match configured chain` | MongoDB holds a projection from a previous genesis — drop the `outbe_*` databases |
| `could not prove genesis formation before DKG round 0` | peers cannot reach each other; check the p2p ports and that the addresses actually route |
| `NodeHost enclave initialization failed` | the enclave was already initialised by an earlier node start; restart enclave and node together with a clear `tee/` |
| `another node appears to be running` (Radicle) | stale control socket; the run script clears it, an older bundle may not |
| `unknown provider '...'` (feeder) | the provider name must come from the feeder's built-in list |
| Enclave `Permission denied` writing sealed state | the sealing directory must be the one baked into the signed manifest |

## Changing a running network

Regenerating the genesis invalidates the OCOMP registrations (they sign its
hash) and every node's stored state. To restart cleanly: stop the services,
delete `validator-N/data`, `validator-N/consensus` and `tee/`, drop the
`outbe_*` MongoDB databases, then deploy the new archives and start again.

## Updating the OCOMP protocol bundle

After the initial launch, OCOMP bundles are upgraded through `Update`; genesis
is not edited. The OCOMP Registry at
`0x000000000000000000000000000000000000EE12` owns active, staged and retiring
authorities. Metadosis only pins each lineage to the bundle active when that
lineage starts.

The transition contract is:

- existing jobs and their retries continue on the predecessor bundle;
- fresh jobs at or after the activation block use the successor;
- every Node and SnapshotExporter has both bundles, and a Worker exists for
  each bundle, before governance;
- activation changes consensus authority without restarting any process.

### Build and preload a successor

Build `outbe-chain`, `outbe-ocomp` and the canonical successor `.ocb1` from one
release revision. Then generate the predecessor-bound proposal artifacts:

```bash
mkdir -p release/protocol-bundles-v1

outbe-chain ocomp successor \
  --genesis /opt/outbe-chain/genesis.json \
  --predecessor-bundle protocol-bundle-v1.ocb1 \
  --successor-bundle protocol-bundle-v2.ocb1 \
  --activation-height "$ACTIVATION_HEIGHT" \
  --update-version "$UPDATE_VERSION" \
  --info "OCOMP V2" \
  --successor-output release/ocomp-successor-v2.ocs1 \
  --proposal-output release/ocomp-update-v2.json \
  --bundle-catalog-dir release/protocol-bundles-v1
```

The command validates the chain identity and predecessor relationship and
writes canonical `OCS1`, ready-to-submit Update JSON, and a non-overwriting
`<successor-hash>.ocb1` catalog entry. A proposal may carry both `teePolicy` and
`ocompSuccessor`; Update stages and activates both atomically.

On every validator, install the successor in the node domain catalog and add
both adjacent hashes to the exporter environment:

```bash
DOMAIN=/opt/outbe-chain/validator-N/ocomp/domain-v1
V1_HASH=0x...
V2_HASH=0x...

sudo install -m 640 \
  "/opt/outbe-chain/protocol-bundles-v1/${V2_HASH#0x}.ocb1" \
  "$DOMAIN/protocol-bundles-v1/${V2_HASH#0x}.ocb1"

printf 'OCOMP_PROTOCOL_BUNDLE_HASHES=%s,%s\n' "$V1_HASH" "$V2_HASH" \
  > /opt/outbe-chain/validator-N/ocomp-bundles.env
printf 'OCOMP_SUCCESSOR_PROTOCOL_BUNDLE_HASH=%s\n' "$V2_HASH" \
  > /opt/outbe-chain/validator-N/ocomp-successor.env
```

Before submitting the proposal, perform one ordinary maintenance restart so
the embedded Supervisor loads both catalog entries, then run the initial and
successor Workers. The successor lane uses the node-derived OCOMP base port
plus six; one SnapshotExporter serves both lanes.

At `activationHeight`, do not restart Node, SnapshotExporter or either Worker.
Verify that the active hash is the successor, the pending predecessor job still
has its original pin, and a fresh job is processed by the successor Worker.

### Retirement and the next upgrade

The Registry exposes `retiringProtocolBundleHash()`,
`liveLineageCount(bundleHash)` and `retentionUntil(bundleHash)`. Remove the
predecessor only after the retiring hash becomes zero. During a normal
maintenance restart, stop its Worker, delete its hash-addressed catalog file,
and remove its hash from `ocomp-bundles.env`.

During that maintenance window also write the surviving active hash to
`ocomp-active.env`. Before the next proposal, append the new successor hash to
`ocomp-bundles.env` and write it to `ocomp-successor.env`.

The populated hash catalog is authoritative. The legacy
`protocol-bundle-v1.ocb1` path is only an initial compatibility fallback and is
ignored once catalog entries exist. Consequently, after V1 retirement the next
transition loads V2 plus V3 rather than permanently forcing V1 into every
runtime.
