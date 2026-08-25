# DCAP testnet launch

This is the operator checklist for the current testnet. It creates one new
four-validator network with chain ID `54322345`, mandatory DCAP from block 1,
and no dev or legacy fallback.

## 1. Prepare the four hosts

On every validator host:

1. Confirm `/dev/sgx_enclave` and `/dev/sgx_provision` exist.
2. Configure PCCS/QCNL and run `scripts/sgx-smoke.sh` successfully as described
   in [local-pccs-for-dcap.md](local-pccs-for-dcap.md).
3. Install the exact release `outbe-chain` binary and Docker.
4. Provide a transaction-capable MongoDB deployment. Each validator uses its
   own logical database; the generated commands set the database name.
5. Pull and verify the exact signed enclave image by digest. A mutable image tag
   is rejected by the network preparer.

The current testnet policy accepts Platform `UpToDate`, `SWHardeningNeeded`, and
`ConfigurationAndSWHardeningNeeded`; QE must be exactly `UpToDate`.

Put the four public validator hosts, one per line, in `testnet-hosts.txt`. Do not
include ports.

## 2. Generate the network once

Run this on a trusted machine. The output contains all four validators' private
keys, so do not generate it on a shared host or commit it to Git.

```bash
RELEASE=/path/to/verified-release
IMAGE='ghcr.io/outbe/outbe-tee-enclave-testnet@sha256:<64-hex-digest>'

MRENCLAVE=$(jq -r '.measurements.mrenclave' \
  "$RELEASE/metadata/testnet-sgx-bundle.json")
MRSIGNER=$(jq -r '.measurements.mrsigner' \
  "$RELEASE/metadata/testnet-sgx-bundle.json")
ISV_PROD_ID=$(jq -r '.measurements.isv_prod_id' \
  "$RELEASE/metadata/testnet-sgx-bundle.json")
ISV_SVN=$(jq -r '.measurements.isv_svn' \
  "$RELEASE/metadata/testnet-sgx-bundle.json")

python3 scripts/prepare_network.py \
  --seed scripts/seed-testnet-lowstake.json \
  --generate-validators 4 \
  --validator-hosts-file testnet-hosts.txt \
  --output-dir /secure/outbe-testnet \
  --runtime-base-dir /var/lib/outbe/testnet \
  --chain-binary "$RELEASE/bin/outbe-chain" \
  --keygen-binary "$RELEASE/bin/outbe-keygen" \
  --runtime-chain-binary /usr/local/bin/outbe-chain \
  --tee-mode dcap-required \
  --chain-id 54322345 \
  --mrenclave "$MRENCLAVE" \
  --mrsigner "$MRSIGNER" \
  --isv-prod-id "$ISV_PROD_ID" \
  --minimum-isv-svn "$ISV_SVN" \
  --minimum-tcb-evaluation-data-number 1 \
  --enclave-image "$IMAGE"
```

The command fails unless it can generate and re-parse the complete OCOMP and
TEE ChainSpec. It also creates fresh OCOMP keys and PoP registrations, the
founding consensus material, unique endpoints, and matching enclave/node
scripts. OCOMP membership is the ordered ACTIVE ValidatorSet. The four-founder
topology therefore starts with `N=4` and quorum `3`; later ACTIVE validators are
included automatically in new jobs, while existing jobs keep their pinned
historical snapshot.

## 3. Copy each validator's files

Copy these common files to `/var/lib/outbe/testnet/` on all four hosts:

```text
genesis.json
reth-bootnodes.txt
protocol-bundle-v1.ocb1
```

On host `N`, also copy only `validator-N/` and the two matching scripts:

```text
commands/enclave-N.sh
commands/validator-N.sh
```

Keep `signing-key.hex`, `evm-key.hex` and `ocomp-key-v1.hex` readable only by
the node user. Fresh bundles intentionally
contain no `signing-share.hex`: the four founders create threshold shares and
the permanent offer key together during the live block-1 ceremony. The shares
are then persisted under each node's `validator-N/data/keys/` directory. Verify
that the common files are byte-identical on every host before starting anything.

## 4. Start the four founders

On host `N`, set the MongoDB URI and start the enclave first:

```bash
export OUTBE_PROJECTION_MONGODB_URI='mongodb://<transaction-capable-mongodb>/'
/var/lib/outbe/testnet/commands/enclave-N.sh
```

The enclave command runs in the foreground; use the host's service manager in a
real deployment. Wait until its generated loopback port is listening, then
start the node in another supervised process:

```bash
export OUTBE_PROJECTION_MONGODB_URI='mongodb://<transaction-capable-mongodb>/'
/var/lib/outbe/testnet/commands/validator-N.sh
```

Start all four founders within the generated 300-second TEE bootstrap window.
Do not run `outbe-cli tee join` for founders: their one-time permanent offer key
is created by the block-0 founding ceremony. No node may process block 1 before
that ceremony succeeds.

## 5. Accept the testnet only after these checks pass

The generated ports are:

| Validator | RPC | TEE |
|---:|---:|---:|
| 0 | 8545 | 17000 |
| 1 | 8546 | 17001 |
| 2 | 8547 | 17002 |
| 3 | 8548 | 17003 |

Run against every validator RPC port:

```bash
curl -sS -H 'content-type: application/json' \
  --data '{"jsonrpc":"2.0","id":1,"method":"eth_blockNumber","params":[]}' \
  http://<validator-host>:<RPC_PORT>
```

All four nodes must report the same non-zero height. Then check the permanent
offer key from each enclave against the chain:

```bash
outbe-cli tee pubkey \
  --enclave-socket 127.0.0.1:1700N \
  --rpc-url http://127.0.0.1:<RPC_PORT> \
  --diff-chain
```

The launch is successful only when all four commands exit zero and consensus
continues advancing. Useful log milestones are:

```text
validated mandatory TEE attestation ChainSpec authority
mandatory TEE enclave sidecar connected before execution launch
permanent offer-key gate passed before threshold work
TEE bootstrap completed before consensus startup
```

If bootstrap fails before block 1, stop all four nodes and enclaves. Fix the
host/PCCS/release problem and start the same untouched bundle again only if no
chain state was finalized. If the founding offer key or its sealed state is
lost, create a new genesis and a new network; there is no recovery or fallback.

## 6. Add a FullNode after block 1

A FullNode is not a founder. Give it the same `genesis.json`, verified release
image and bootnodes; provision its persistent Reth P2P key plus independent
EVM/BLS key files, then run its DCAP enclave. The FullNode runtime does not load
the EVM/BLS files and their presence grants no ValidatorSet role:

```bash
mkdir -p /var/lib/outbe/testnet/fullnode/tee
outbe-keygen hybrid --output-dir /var/lib/outbe/testnet/fullnode/keys
docker run --rm --network host \
  --device /dev/sgx_enclave:/dev/sgx_enclave \
  --device /dev/sgx_provision:/dev/sgx_provision \
  -v /var/lib/outbe/testnet/fullnode/tee:/var/lib/outbe/tee \
  "$IMAGE" \
  --socket 127.0.0.1:17000 \
  --tee-dir /var/lib/outbe/tee \
  --chain-id 0x00000000000000000000000000000000000000000000000000000000033ce4a9
```

Before starting the node, register that enclave once:

```bash
BINDING_ID=0x$(openssl rand -hex 32)
LATEST_TS_HEX=$(curl -fsS -H 'content-type: application/json' \
  --data '{"jsonrpc":"2.0","id":1,"method":"eth_getBlockByNumber","params":["latest",false]}' \
  http://<certified-validator-rpc>:8545 | jq -r '.result.timestamp')
VALID_UNTIL=$((LATEST_TS_HEX + 7200))

outbe-cli tee join \
  --enclave-socket 127.0.0.1:17000 \
  --node-data-dir /var/lib/outbe/testnet/fullnode/data \
  --reth-p2p-secret-key /var/lib/outbe/testnet/fullnode/reth-p2p-secret.hex \
  --node-evm-key /var/lib/outbe/testnet/fullnode/keys/evm-key.hex \
  --binding-id "$BINDING_ID" \
  --valid-until "$VALID_UNTIL" \
  --private-key "$FUNDED_RELAY_KEY" \
  --rpc-url http://<certified-validator-rpc>:8545
```

Only after `tee join` succeeds, start it:

```bash
: "${OUTBE_PROJECTION_MONGODB_URI:?set OUTBE_PROJECTION_MONGODB_URI}"

# The p2p key is passed as a FILE, never inline hex in argv (`ps` is
# world-readable). reth parses the file without trimming — normalize it once.
printf '%s' "$(tr -d '[:space:]' < /var/lib/outbe/testnet/fullnode/reth-p2p-secret.hex)" \
  > /var/lib/outbe/testnet/fullnode/reth-p2p-secret.hex

/usr/local/bin/outbe-chain node \
  --chain /var/lib/outbe/testnet/genesis.json \
  --datadir /var/lib/outbe/testnet/fullnode/data \
  --consensus.storage-dir /var/lib/outbe/testnet/fullnode/consensus \
  --engine.persistence-threshold 0 \
  --engine.memory-block-buffer-target 0 \
  --p2p-secret-key /var/lib/outbe/testnet/fullnode/reth-p2p-secret.hex \
  --bootnodes "$(paste -sd, /var/lib/outbe/testnet/reth-bootnodes.txt)" \
  --upstream http://<certified-validator-rpc>:<RPC_PORT> \
  --http --http.addr 127.0.0.1 --http.port 8545 \
  --http.api eth,net,web3,outbe \
  --projection.mongodb-uri "$OUTBE_PROJECTION_MONGODB_URI" \
  --projection.mongodb-database outbe_testnet_fullnode_0 \
  --ocomp.supervisor-socket /opt/outbe-chain/ocomp/run/node-supervisor.sock \
  --ocomp.snapshot-exporter-socket /opt/outbe-chain/ocomp/run/node-snapshot-exporter.sock \
  --ocomp.protocol-bundle-hash <0x-protocol-bundle-hash> \
  --ocomp.boot-nonce <nonzero-0x32-byte-boot-nonce> \
  --tee-enclave-socket 127.0.0.1:17000
```

A FullNode never receives a founding consensus share or an OCOMP signing key.
It must run the same SnapshotExporter/worker services as a validator and the
keyless `outbe-ocomp follower` role under the matching deployment identity. The
follower independently executes Lysis and commits its canonical result locally;
it does not read `OUTBE_OCOMP_RPC_URL` and cannot submit a vote. At the first
quorum-forming vote, the node waits for that durable result and fails closed on
any digest/root/manifest mismatch.
