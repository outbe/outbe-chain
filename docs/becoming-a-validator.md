# Run an Outbe FullNode and promote it to a Validator

This runbook joins an existing Outbe network from a fresh Linux host:

1. install a verified Outbe release;
2. start and synchronize a non-voting FullNode;
3. register the operator and restart the same node as a Validator;
4. stake, confirm readiness, and wait for DKG activation.

The procedure was tested on Ubuntu 24.04 LTS x86_64. Other Linux distributions
may be used with equivalent packages, Docker, SGX devices, and the
release-pinned Gramine runtime.

Run the node as a regular operator account with `sudo` access. Do not build the
release on the node host and do not run `outbe-chain` itself as root. Commands
assume Bash and are intended to be run in the same shell. After a new SSH login,
reload `/etc/outbe-chain/network.env` before continuing.

> **TEE identity during promotion**
>
> TEE V1 admission is role-neutral. A node performs `tee join` exactly once,
> before its first FullNode startup. Promotion keeps the same Reth P2P identity,
> NodeHost manifest, enclave, sealed state, permanent offer key, and synchronized
> datadir. Do not repeat `tee join` or replace TEE/NodeHost state when enabling
> Validator mode.

## 0. Collect the network inputs

Get these from a trusted network operator:

- the exact immutable Outbe release tag used by the network;
- `genesis.json`;
- `reth-bootnodes.txt`, one `enode://...` URL per line;
- a trusted RPC URL with continuous `outbe_getFinalization` history;
- the Gramine/SGX runtime required by the release;
- immediately before Validator startup, the current
  `dkg_polynomial.hex`, `dkg_output.hex`, and `consensus-peers.txt`;
- an address reachable in both directions between validators on TCP `30400`.

`consensus-peers.txt` contains one remote bootstrap peer per line. Do not add
the new validator itself:

```text
<96-hex-character-BLS-public-key>@host:30400
```

The DKG polynomial and output are public bootstrap data. Never copy another
validator's `dkg_share.hex`, BLS/EVM private keys, Reth P2P key, OCOMP key, or
sealed enclave state.

This guide uses:

| Path | Purpose |
| --- | --- |
| `/opt/outbe-chain` | binaries and network files |
| `/var/lib/outbe/node` | Reth data and NodeHost state |
| `/var/lib/outbe/keys` | this validator's BLS, EVM, and DKG files |
| `/var/lib/outbe/consensus` | consensus state |
| `/var/lib/outbe/ocomp/domain-v1` | embedded OCOMP state and keys |
| `/var/lib/outbe/tee` | sealed enclave state |
| `/var/lib/outbe/mongodb` | MongoDB projection data |

## 1. Prepare the host

Install the equivalent of these tools with your distribution's package
manager: CA certificates, `curl`, Docker, `jq`, `openssl`, `tar`, `sha256sum`,
Cosign, and Foundry `cast`.

Ubuntu 24.04 example:

```bash
sudo apt update
sudo apt install -y ca-certificates curl docker.io jq openssl tar
sudo systemctl enable --now docker
```

Install version-pinned Cosign using an operator-approved Sigstore or
distribution package. Install Foundry for `cast`:

```bash
curl -L https://foundry.paradigm.xyz | bash
export PATH="$HOME/.foundry/bin:$PATH"
foundryup
```

The host must expose SGX and provide the Gramine version required by the
release:

```bash
ls -l /dev/sgx_enclave /dev/sgx_provision
command -v gramine-sgx
```

These checks only confirm that SGX and the launcher are present. The network's
genesis policy decides whether `GramineDirectDev` is accepted or DCAP is
required. For DCAP deployments, also follow
[`dcap-testnet-launch.md`](dcap-testnet-launch.md).

Configure host and perimeter firewalls according to your infrastructure policy.
Keep MongoDB (`27017`), the enclave (`7000`), and operator RPC (`8545`) on
loopback. Expose the configured Reth P2P ports and allow bidirectional Validator
traffic on TCP `30400`.

## 2. Install the verified release

Use the exact tag supplied by the operator, never a moving `latest` release:

```bash
OUTBE_RELEASE='<operator-supplied-immutable-release-tag>'
RELEASE_URL="https://github.com/outbe/outbe-chain/releases/download/$OUTBE_RELEASE"
RELEASE_DIR=$(mktemp -d "/tmp/outbe-${OUTBE_RELEASE}.XXXXXXXX")

cd "$RELEASE_DIR"

for ASSET in \
  ReleaseManifest.json \
  ReleaseManifest.sigstore.json \
  outbe-linux-x86_64.tar \
  outbe-tee-enclave-sgx.tar
do
  curl --fail --location --retry 3 \
    --proto '=https' --proto-redir '=https' --tlsv1.2 \
    --output "$ASSET" \
    "$RELEASE_URL/$ASSET"
done
```

Verify the signed manifest and bind it to the requested tag and platform:

```bash
cosign verify-blob \
  --bundle ReleaseManifest.sigstore.json \
  --certificate-identity-regexp \
    '^https://github.com/outbe/outbe-chain/.github/workflows/testnet-release.yml@refs/heads/main$' \
  --certificate-oidc-issuer \
    'https://token.actions.githubusercontent.com' \
  ReleaseManifest.json

test "$(jq -er '.release.tag' ReleaseManifest.json)" = "$OUTBE_RELEASE"
test "$(jq -er '.release.lifecycle' ReleaseManifest.json)" = verified
test "$(jq -er '.build.target' ReleaseManifest.json)" = x86_64-unknown-linux-gnu

SGX_SHA=$(jq -er '
  .artifacts[]
  | select(.name == "outbe-tee-enclave-sgx-bundle")
  | .digest.value
' ReleaseManifest.json)

printf '%s  outbe-tee-enclave-sgx.tar\n' "$SGX_SHA" | sha256sum -c -
```

Extract the release and verify every released ELF against the signed manifest:

```bash
mkdir payload sgx
tar --no-same-owner --no-same-permissions -xf outbe-linux-x86_64.tar -C payload
tar --no-same-owner --no-same-permissions -xf outbe-tee-enclave-sgx.tar -C sgx

jq -er '
  .artifacts[]
  | select(.kind == "elf")
  | "\(.digest.value)  \(.path)"
' ReleaseManifest.json > ELF.SHA256SUMS

(cd payload && sha256sum -c ../ELF.SHA256SUMS)
```

Install the node tools and signed enclave bundle:

```bash
sudo install -d -o root -g root -m 0755 \
  /opt/outbe-chain \
  /opt/outbe-chain/release

sudo install -o root -g root -m 0755 \
  payload/bin/outbe-chain \
  payload/bin/outbe-cli \
  payload/bin/outbe-keygen \
  sgx/rootfs/opt/outbe/sgx/bin/outbe-tee-enclave \
  /opt/outbe-chain/

sudo install -o root -g root -m 0644 \
  sgx/rootfs/opt/outbe/sgx/outbe-tee-enclave.manifest \
  sgx/rootfs/opt/outbe/sgx/outbe-tee-enclave.manifest.sgx \
  sgx/rootfs/opt/outbe/sgx/outbe-tee-enclave.sig \
  /opt/outbe-chain/

sudo install -o root -g root -m 0644 \
  ReleaseManifest.json \
  ReleaseManifest.sigstore.json \
  ELF.SHA256SUMS \
  /opt/outbe-chain/release/

printf '%s\n' "$OUTBE_RELEASE" \
  | sudo tee /opt/outbe-chain/release/RELEASE_TAG >/dev/null
```

Do not modify, locally rebuild, re-sign, or mix enclave bundle files from
different releases. See [`testnet-sgx-release.md`](testnet-sgx-release.md) for
the complete release-evidence procedure.

Install the network files supplied by the operator:

```bash
NETWORK_BUNDLE_DIR='<operator-supplied-network-bundle-directory>'

sudo install -o root -g root -m 0644 \
  "$NETWORK_BUNDLE_DIR/genesis.json" \
  "$NETWORK_BUNDLE_DIR/reth-bootnodes.txt" \
  /opt/outbe-chain/
```

## 3. Configure the node and create its keys

Create a non-secret environment file and replace the network-specific values:

```bash
sudo install -d -o root -g root -m 0755 /etc/outbe-chain

sudo tee /etc/outbe-chain/network.env >/dev/null <<'EOF'
APP_DIR=/opt/outbe-chain
BASE_DIR=/var/lib/outbe
NODE_DATA_DIR=/var/lib/outbe/node
KEYS_DIR=/var/lib/outbe/keys
CONSENSUS_DIR=/var/lib/outbe/consensus
OCOMP_DIR=/var/lib/outbe/ocomp/domain-v1
TEE_DIR=/var/lib/outbe/tee
NETWORK_RPC=https://rpc.testnet.outbe.net
ADVERTISE_IP=REPLACE_ME
PROJECTION_DB=outbe_testnet_validator_0
MONGODB_URI='mongodb://127.0.0.1:27017/?replicaSet=rs0&directConnection=true'
TEE_ENDPOINT=127.0.0.1:7000
TEE_LEASE_SECONDS=86400
EOF

sudoedit /etc/outbe-chain/network.env

set -a
source /etc/outbe-chain/network.env
set +a
```

The exact `NODE_DATA_DIR` value is used both for `tee join` and
`outbe-chain node --datadir`.

Create persistent directories:

```bash
sudo install -d -o "$USER" -g "$(id -gn)" -m 0700 \
  "$BASE_DIR" \
  "$NODE_DATA_DIR" \
  "$KEYS_DIR" \
  "$CONSENSUS_DIR" \
  "$OCOMP_DIR" \
  "$BASE_DIR/mongodb"

sudo install -d -o root -g root -m 0700 "$TEE_DIR"
```

Provision the future Validator EVM/BLS keys now, even though FullNode runtime
does not load them. The same EVM key also authorizes the one-time association
between this address and the node's role-neutral NodeHost identity.

```bash
cd "$APP_DIR"
umask 077

./outbe-keygen hybrid --output-dir "$KEYS_DIR"
openssl rand -hex 32 > "$BASE_DIR/reth-p2p-secret.hex"

BLS_PUBKEY=$(
  ./outbe-keygen show-pubkey --key "$KEYS_DIR/signing-key.hex" \
    | awk '/public key:/ {print $3}'
)

EVM_KEY="0x$(tr -d '[:space:]' < "$KEYS_DIR/evm-key.hex")"
VALIDATOR_ADDR=$(cast wallet address --private-key "$EVM_KEY")
unset EVM_KEY

printf 'VALIDATOR_ADDR=%s\nBLS_PUBKEY=%s\n' \
  "$VALIDATOR_ADDR" "$BLS_PUBKEY"
```

Back up `signing-key.hex` and `evm-key.hex` securely. Fund
`$VALIDATOR_ADDR` with at least the network minimum stake plus transaction gas.

Confirm that the local genesis belongs to the RPC network, then check the
funded balance in base units:

```bash
CHAIN_ID=$(jq -r '.config.chainId' "$APP_DIR/genesis.json")
test "$CHAIN_ID" = "$(cast chain-id --rpc-url "$NETWORK_RPC")" \
  || { echo 'ABORT: genesis and RPC chain IDs differ'; exit 1; }

cast balance "$VALIDATOR_ADDR" --rpc-url "$NETWORK_RPC"
```

Never run key commands with shell tracing enabled. The CLI signs locally, but a
private key passed through `--private-key` can briefly be visible to privileged
users in the host process list.

## 4. Start MongoDB

Projection writes require a MongoDB replica set:

```bash
sudo docker run -d \
  --name outbe-mongodb \
  --restart unless-stopped \
  -p 127.0.0.1:27017:27017 \
  -v "$BASE_DIR/mongodb:/data/db" \
  mongo:8.3.7 \
  --replSet rs0 \
  --bind_ip_all

until sudo docker exec outbe-mongodb mongosh --quiet \
  --eval 'db.runCommand({ping:1}).ok' >/dev/null 2>&1
do
  sleep 1
done

sudo docker exec outbe-mongodb mongosh --quiet --eval \
  "rs.initiate({_id:'rs0',members:[{_id:0,host:'127.0.0.1:27017'}]})"

until sudo docker exec outbe-mongodb mongosh --quiet \
  --eval 'db.hello().isWritablePrimary ? quit(0) : quit(1)' \
  >/dev/null 2>&1
do
  sleep 1
done
```

## 5. Start and synchronize the FullNode

### 5.1 Start the enclave

```bash
set -a
source /etc/outbe-chain/network.env
set +a

CHAIN_ID=$(jq -r '.config.chainId' "$APP_DIR/genesis.json")
printf -v CHAIN_ID_HEX '0x%064x' "$CHAIN_ID"

cd "$APP_DIR"

sudo nohup gramine-sgx outbe-tee-enclave \
  --tee-dir "$TEE_DIR" \
  --socket "$TEE_ENDPOINT" \
  --chain-id "$CHAIN_ID_HEX" \
  > "$BASE_DIR/tee-enclave.log" 2>&1 &

TEE_PID=$!
printf '%s\n' "$TEE_PID" | sudo tee "$BASE_DIR/tee-enclave.pid" >/dev/null

sleep 5
sudo tail -n 30 "$BASE_DIR/tee-enclave.log"
```

Do not continue unless the log says that the enclave is listening and sealing
is enabled. An unattested/debug warning is acceptable only when the genesis
policy explicitly permits it.

### 5.2 Perform the one-time TEE join

The node's EVM key is used below as the funded relay as well. The relay may be a
different funded account, but `--node-evm-key` must always point to this node's
persistent EVM key file.

```bash
EVM_KEY="0x$(tr -d '[:space:]' < "$KEYS_DIR/evm-key.hex")"
BINDING_ID="0x$(openssl rand -hex 32)"
LATEST_TS_HEX=$(
  cast block latest --json --rpc-url "$NETWORK_RPC" \
    | jq -r '.timestamp'
)
VALID_UNTIL=$((LATEST_TS_HEX + TEE_LEASE_SECONDS))

./outbe-cli tee join \
  --enclave-socket "$TEE_ENDPOINT" \
  --node-data-dir "$NODE_DATA_DIR" \
  --reth-p2p-secret-key "$BASE_DIR/reth-p2p-secret.hex" \
  --node-evm-key "$KEYS_DIR/evm-key.hex" \
  --binding-id "$BINDING_ID" \
  --valid-until "$VALID_UNTIL" \
  --private-key "$EVM_KEY" \
  --rpc-url "$NETWORK_RPC" \
  --timeout-secs 60

unset EVM_KEY
```

Continue only after the command reports that the offer key was durably installed
and the authenticated enclave connection reopened. Do not run `tee join` again
when this node becomes a Validator.

### 5.3 Start the FullNode

```bash
BOOTNODES=$(
  grep -Ev '^[[:space:]]*(#|$)' reth-bootnodes.txt \
    | paste -sd, -
)

nohup ./outbe-chain node \
  --chain "$APP_DIR/genesis.json" \
  --datadir "$NODE_DATA_DIR" \
  --p2p-secret-key "$BASE_DIR/reth-p2p-secret.hex" \
  --bootnodes "$BOOTNODES" \
  --nat "extip:$ADVERTISE_IP" \
  --port 30303 \
  --discovery.port 30303 \
  --discovery.v5.addr 0.0.0.0 \
  --discovery.v5.port 31303 \
  --upstream "$NETWORK_RPC" \
  --projection.mongodb-uri "$MONGODB_URI" \
  --projection.mongodb-database "$PROJECTION_DB" \
  --engine.persistence-threshold 0 \
  --engine.memory-block-buffer-target 0 \
  --http \
  --http.addr 127.0.0.1 \
  --http.port 8545 \
  --http.api eth,net,web3,outbe \
  --tee-enclave-socket "$TEE_ENDPOINT" \
  --tee-session-mode production-node-host \
  --consensus.storage-dir "$CONSENSUS_DIR" \
  > "$BASE_DIR/full-node.log" 2>&1 &

printf '%s\n' "$!" > "$BASE_DIR/full-node.pid"
```

OCOMP runs inside `outbe-chain`; no separate `outbe-ocomp` process is required.
For a `DcapRequired` network, add the renewal arguments required by
[`dcap-testnet-launch.md`](dcap-testnet-launch.md), including a funded
`--tee-renewal.relay-key`.

Follow synchronization:

```bash
tail -f "$BASE_DIR/full-node.log"
```

In another shell, load the environment and compare heights:

```bash
set -a
source /etc/outbe-chain/network.env
set +a

LOCAL=$(cast block-number --rpc-url http://127.0.0.1:8545)
REMOTE=$(cast block-number --rpc-url "$NETWORK_RPC")
printf 'local=%s remote=%s behind=%s\n' "$LOCAL" "$REMOTE" "$((REMOTE - LOCAL))"
```

When the lag is zero or one block, compare one exact block. This guards against
quietly following the wrong chain:

```bash
H=$(cast block-number --rpc-url http://127.0.0.1:8545)
LOCAL_HASH=$(
  cast block "$H" --json --rpc-url http://127.0.0.1:8545 \
    | jq -r '.hash'
)
REMOTE_HASH=$(
  cast block "$H" --json --rpc-url "$NETWORK_RPC" \
    | jq -r '.hash'
)

test "$LOCAL_HASH" = "$REMOTE_HASH" \
  && echo 'FullNode synchronized: OK' \
  || { echo 'ABORT: block hash mismatch'; exit 1; }
```

## 6. Prepare the Validator

Do this only after the FullNode is synchronized.

### 6.1 Install the current DKG bootstrap files and peers

Obtain fresh copies from an active validator's actual
`--consensus.keys-dir`, then install them as:

```text
/opt/outbe-chain/dkg_polynomial.hex
/opt/outbe-chain/dkg_output.hex
/opt/outbe-chain/consensus-peers.txt
```

Do not copy `dkg_share.hex`. The new Validator receives its own share during a
later DKG reshare.

### 6.2 Register the Validator and consensus address

```bash
cd "$APP_DIR"

CHAIN_ID=$(jq -r '.config.chainId' "$APP_DIR/genesis.json")
EVM_KEY="0x$(tr -d '[:space:]' < "$KEYS_DIR/evm-key.hex")"
VALIDATOR_ADDR=$(cast wallet address --private-key "$EVM_KEY")
BLS_PUBKEY=$(
  ./outbe-keygen show-pubkey --key "$KEYS_DIR/signing-key.hex" \
    | awk '/public key:/ {print $3}'
)
BLS_SIG=$(
  ./outbe-keygen sign-registration \
    --key "$KEYS_DIR/signing-key.hex" \
    --validator-address "$VALIDATOR_ADDR" \
    --chain-id "$CHAIN_ID" \
    | awk '/signature:/ {print $2}'
)

./outbe-cli validator register \
  --pubkey "0x$BLS_PUBKEY" \
  --bls-sig "0x$BLS_SIG" \
  --private-key "$EVM_KEY" \
  --rpc-url "$NETWORK_RPC"
```

Wait until the transaction is included and this command reports
`Registered`:

```bash
./outbe-cli validator info "$VALIDATOR_ADDR" --rpc-url "$NETWORK_RPC"
```

Then publish the address reachable by the consensus mesh:

```bash
./outbe-cli validator set-p2p \
  --symmetric "$ADVERTISE_IP:30400" \
  --private-key "$EVM_KEY" \
  --rpc-url "$NETWORK_RPC"
```

### 6.3 Create the Validator OCOMP identity

```bash
GENESIS_HASH=$(cast block 0 --json --rpc-url "$NETWORK_RPC" | jq -r '.hash')

./outbe-keygen ocomp \
  --output-dir "$OCOMP_DIR" \
  --chain-id "$CHAIN_ID" \
  --genesis-hash "$GENESIS_HASH" \
  --validator-address "$VALIDATOR_ADDR" \
  --consensus-bls-min-pk "$BLS_PUBKEY"

install -m 0600 \
  "$KEYS_DIR/evm-key.hex" \
  "$OCOMP_DIR/ocomp-evm-key.hex"

unset EVM_KEY BLS_SIG
```

Preserve the entire OCOMP directory. Its private key and sign-once state must
survive restarts.

## 7. Promote the FullNode to a Validator

### 7.1 Stop only the FullNode process

Leave MongoDB, the enclave, `$TEE_DIR`, and
`$NODE_DATA_DIR/tee-node-host-v1` unchanged:

```bash
FULL_NODE_PID=$(cat "$BASE_DIR/full-node.pid")
kill -TERM "$FULL_NODE_PID"
while kill -0 "$FULL_NODE_PID" 2>/dev/null; do sleep 1; done
```

### 7.2 Start the Validator

The public DKG pair lets the new node follow and verify consensus without a
private share. It receives its own share at the next reshare.

```bash
BOOTNODES=$(
  grep -Ev '^[[:space:]]*(#|$)' reth-bootnodes.txt \
    | paste -sd, -
)
CONSENSUS_PEERS=$(
  grep -Ev '^[[:space:]]*(#|$)' consensus-peers.txt \
    | paste -sd, -
)

nohup ./outbe-chain node --validator \
  --chain "$APP_DIR/genesis.json" \
  --datadir "$NODE_DATA_DIR" \
  --p2p-secret-key "$BASE_DIR/reth-p2p-secret.hex" \
  --bootnodes "$BOOTNODES" \
  --nat "extip:$ADVERTISE_IP" \
  --port 30303 \
  --discovery.port 30303 \
  --discovery.v5.addr 0.0.0.0 \
  --discovery.v5.port 31303 \
  --projection.mongodb-uri "$MONGODB_URI" \
  --projection.mongodb-database "$PROJECTION_DB" \
  --engine.persistence-threshold 0 \
  --engine.memory-block-buffer-target 0 \
  --http \
  --http.addr 127.0.0.1 \
  --http.port 8545 \
  --http.api eth,net,web3,outbe \
  --tee-enclave-socket "$TEE_ENDPOINT" \
  --tee-session-mode production-node-host \
  --consensus.signing-key "$KEYS_DIR/signing-key.hex" \
  --validator.evm-key "$KEYS_DIR/evm-key.hex" \
  --consensus.public-polynomial "$APP_DIR/dkg_polynomial.hex" \
  --consensus.dkg-output "$APP_DIR/dkg_output.hex" \
  --consensus.keys-dir "$KEYS_DIR" \
  --consensus.listen-addr 0.0.0.0:30400 \
  --consensus.storage-dir "$CONSENSUS_DIR" \
  --consensus.peers "$CONSENSUS_PEERS" \
  --consensus.use-local-defaults \
  > "$BASE_DIR/validator.log" 2>&1 &

printf '%s\n' "$!" > "$BASE_DIR/validator.pid"
```

`--consensus.use-local-defaults` is appropriate for the routed private testnet
used to verify this guide. Follow the network operator's setting on another
network. Do not pass `--upstream` to a Validator.

The first startup should report verifier mode because this Validator does not
have a threshold share yet:

```bash
sleep 15
tail -n 50 "$BASE_DIR/validator.log"
cast rpc outbe_syncStatus --rpc-url http://127.0.0.1:8545
cast rpc outbe_consensusStatus --rpc-url http://127.0.0.1:8545
```

## 8. Stake, confirm readiness, and activate

First make sure the local Validator is at the network tip:

```bash
LOCAL=$(cast block-number --rpc-url http://127.0.0.1:8545)
REMOTE=$(cast block-number --rpc-url "$NETWORK_RPC")
printf 'local=%s remote=%s behind=%s\n' "$LOCAL" "$REMOTE" "$((REMOTE - LOCAL))"
```

Stake the current network minimum. `--amount` uses integer base units:

```bash
EVM_KEY="0x$(tr -d '[:space:]' < "$KEYS_DIR/evm-key.hex")"
VALIDATOR_ADDR=$(cast wallet address --private-key "$EVM_KEY")
MIN_STAKE_HEX=$(cast storage \
  0x000000000000000000000000000000000000EE02 \
  0 \
  --rpc-url "$NETWORK_RPC")
MIN_STAKE_WEI=$(cast to-dec "$MIN_STAKE_HEX")

./outbe-cli staking stake \
  --validator "$VALIDATOR_ADDR" \
  --amount "$MIN_STAKE_WEI" \
  --private-key "$EVM_KEY" \
  --rpc-url "$NETWORK_RPC"
```

After the transaction is included, the Validator should be `Pending`:

```bash
./outbe-cli validator info "$VALIDATOR_ADDR" --rpc-url "$NETWORK_RPC"
```

While the local node remains at the network tip, submit its OCOMP registration
and readiness signal:

```bash
./outbe-cli validator confirm-ready \
  --registration "$OCOMP_DIR/ocomp-registration-v1.ocb1" \
  --private-key "$EVM_KEY" \
  --rpc-url "$NETWORK_RPC"

unset EVM_KEY
```

The Validator remains `Pending` until the next scheduled DKG reshare activates
it. Do not restart it merely because activation is not immediate.

After the next DKG boundary, run the final checks:

```bash
./outbe-cli validator info "$VALIDATOR_ADDR" --rpc-url "$NETWORK_RPC"
./outbe-chain dkg status --storage-dir "$KEYS_DIR"
./outbe-cli monitor readiness \
  --address "$VALIDATOR_ADDR" \
  --rpc-url http://127.0.0.1:8545
```

The setup is complete when:

- Validator status is `Active`;
- `Has BLS Share` is `true`;
- DKG status is `READY`;
- readiness reports synchronized execution, active consensus, and peers.

## 9. Persistence and normal restarts

Back up and preserve:

- `$KEYS_DIR`, including the DKG files written after activation;
- `$BASE_DIR/reth-p2p-secret.hex`;
- `$TEE_DIR` and `$NODE_DATA_DIR/tee-node-host-v1` together;
- `$OCOMP_DIR`, including its sign-once state;
- `$NODE_DATA_DIR`, `$CONSENSUS_DIR`, and `$BASE_DIR/mongodb`.

On a normal restart:

1. start MongoDB;
2. start the enclave with the same signed bundle and existing `$TEE_DIR`;
3. start the node in its current role with the same datadir and key paths.

Do not run `tee join`, register, stake, or confirm readiness again. Those are
onboarding operations, not normal startup steps. A returning Active Validator
loads its existing share from `$KEYS_DIR` and resumes signing.

For TEE lease renewal, DCAP rollout, enclave upgrades, consensus recovery, and
Validator lifecycle operations, use the release-matched runbooks and CLI help:

```bash
./outbe-cli tee --help
./outbe-cli validator --help
./outbe-cli staking --help
```

Do not delete consensus, DKG, OCOMP, NodeHost, or sealed TEE state to work
around a startup error.
