# Launching TEE networks

Outbe separates two choices that must not be conflated:

1. The genesis-fixed attestation policy (`DcapRequired` or
   `GramineDirectDev`).
2. The local enclave runtime/session (`gramine-sgx` plus production NodeHost,
   or the development transport).

Supported profiles are:

| Chain policy | Local enclave | Remote attestation | Intended use |
| --- | --- | --- | --- |
| `DcapRequired` | production `outbe-tee-enclave`, `gramine-sgx`, NodeHost | DCAP | testnet/production admission |
| `GramineDirectDev` | production `outbe-tee-enclave`, `gramine-sgx`, NodeHost | none | real SGX development network without Intel collateral |
| `GramineDirectDev` | production or mock enclave, `gramine-direct`, development session | none | hardware-free development |
| `GramineDirectDev` | mock enclave as a native host process, development session | none | development on hosts where Gramine cannot run |

The fourth profile exists because the Gramine test image is published for
`linux/amd64` only and does not survive emulation, so no Apple Silicon host can
run it. It executes the same mock enclave binary with the same arguments, but
directly on the host: no container, no LibOS, no manifest and no signing key.
The Rust localnet harness selects it automatically on every non-Linux host and
labels it `mock-native`; Linux keeps the containerized `mock` profile. It is a
separate named profile, not a fallback — nothing that requires `@gramine-direct`
or `@sgx-no-attest` is satisfied by it, and it records no Gramine image
identity. `run-testnet.sh` does not offer it at all.

The second profile is intentionally supported. It executes inside real SGX,
uses the SGX local report, EGETKEY-backed sealing and the production NodeHost
authorization protocol. It does **not** produce a quote, validate Intel TCB
collateral or claim remote hardware attestation; on chain it remains
`GramineDirectDev` evidence. There is no automatic fallback between profiles.

The two chain policies remain distinct:

- `DcapRequired` is the testnet Intel SGX x86_64 mode on chain ID `54322345`.
  It requires the V1 manifest from block 1, an authorized SGX enclave, quote
  and canonical collateral. A missing or rejected dependency stops startup;
  it never falls back to development mode.
- `GramineDirectDev` accepts development evidence. That policy can be exercised
  either without hardware or by the production SGX-without-DCAP profile above;
  neither is DCAP or release-attestation evidence.

Every node role requires `--tee-enclave-socket`. A Validator cannot start
threshold work or consensus without the permanent resident offer key, except
while a proven fresh block-1 founder is creating that key. A FullNode must have
the exact permanent key and match it against its selected certified upstream
before Reth networking, RPC, sync or execution launches.

## Current release status

The A0 code path makes `teeAttestationV1` and OST3 mandatory and fail-closed.
That alone does not make a testnet release eligible. Testnet activation remains
blocked until the remaining I9 gates prove all of the following for one exact
artifact set:

- Intel QVL `1.26.100.1-noble1`, Gramine `1.9`, the `native-dcap` feature and
  every trusted native artifact are frozen and digest-verified;
- fresh accepted Processor-CA release evidence passes the enclave-resident
  public verifier; any real Platform-CA node must independently pass that same
  verifier when it joins;
- on the same SGX server, exact-release `gramine-sgx` QVL and the maximum
  reachable full-block workload fit the consensus timing budget;
- reachable real Validator and FullNode `DcapRequired` paths are green. A
  logical or physical 32-validator network is not a release gate.

The checked-in B1 candidate now declares `sgx.remote_attestation = "dcap"` and
builds the enclave with exactly `native-dcap`. It still must not be deployed to
the `DcapRequired` testnet until B1 binds the reproducible artifact set; H1, P1
and E1 own hardware acceptance, performance and testnet E2E.

## Run the isolated development network

This is the supported hardware-free operator path. It always creates a
`GramineDirectDev` genesis and always launches one enclave per validator.

Prerequisites are Docker, a transaction-capable MongoDB replica set, and the
Rust toolchain:

```sh
cargo build -p outbe-chain --bin outbe-chain
cargo build -p outbe-cli --bin outbe-cli
cargo build -p outbe-tee-enclave --bin outbe-tee-enclave

docker build \
  -f bin/outbe-tee-enclave/gramine/Dockerfile.test \
  -t outbe-tee-enclave-gramine-test \
  bin/outbe-tee-enclave/gramine
```

Create and start a four-validator development chain:

```sh
./scripts/bootstrap-testnet.sh \
  4 /tmp/outbe-devnet scripts/seed-testnet-lowstake.json

OUTBE_PROJECTION_MONGODB_URI='mongodb://127.0.0.1:27017/?replicaSet=rs0' \
OUTBE_TEE_ENCLAVE_BINARY="$PWD/target/debug/outbe-tee-enclave" \
  ./scripts/run-testnet.sh start /tmp/outbe-devnet

./scripts/run-testnet.sh status /tmp/outbe-devnet
```

`bootstrap-testnet.sh` inserts `teeAttestationV1` only after all other genesis
mutations. `run-testnet.sh` refuses a missing enclave, a bare-host enclave and
any implicit SGX selection. Even on an SGX-capable host this lane deliberately
uses `gramine-direct`.

For deterministic restart testing, use the explicitly test-only mock binary and
its stable test sealing key:

```sh
cargo build -p outbe-tee-enclave \
  --bin outbe-tee-enclave-mock --features mock

OUTBE_PROJECTION_MONGODB_URI='mongodb://127.0.0.1:27017/?replicaSet=rs0' \
OUTBE_TEE_ENCLAVE_MOCK=1 OUTBE_TEE_SEAL=1 \
OUTBE_TEE_ENCLAVE_BINARY="$PWD/target/debug/outbe-tee-enclave-mock" \
  ./scripts/run-testnet.sh start /tmp/outbe-devnet
```

This mock restart case is development reachability evidence only. The
non-mock enclave under `gramine-direct` cannot use EGETKEY sealing, so a fresh
development genesis is required after its identity is lost.

Stop the network with:

```sh
./scripts/run-testnet.sh stop /tmp/outbe-devnet
```

## Run production TEE under SGX without DCAP

Use the E2E/localnet harness mode `sgx-no-attest`. It passes the SGX devices,
renders the Gramine manifest with `sgx.remote_attestation = "none"`, starts the
production enclave with `gramine-sgx`, and starts every node with the production
NodeHost session while keeping the chain policy `GramineDirectDev`:

```sh
cargo run -p outbe-e2e-harness --bin outbe-e2e -- \
  --tee sgx-no-attest \
  --validators 4 \
  --all
```

This requires `/dev/sgx_enclave` (or `/dev/sgx/enclave`) and an accessible SGX
provisioning device. Absence of SGX is fatal for this mode; it never falls back
to `gramine-direct`. The launcher does not mount QVL or require DCAP libraries
for this mode. Selecting `DcapRequired` still requires DCAP and cannot use this
path.

## Construct the testnet ChainSpec

Use only measurements and TCB values taken from the exact frozen release. The
command below validates and writes a ChainSpec; it does not prove that the
release passed hardware gates:

```sh
target/debug/outbe-chain tee genesis \
  --input /path/to/final-seeded-genesis.json \
  --output /path/to/testnet-genesis.json \
  --mode dcap-required \
  --mrenclave 0x<exact-32-byte-release-measurement> \
  --mrsigner 0x<exact-32-byte-release-signer> \
  --isv-prod-id <exact-release-product-id> \
  --minimum-isv-svn <reviewed-minimum-svn> \
  --minimum-tcb-evaluation-data-number <reviewed-nonzero-tcb-number>
```

The command refuses zero measurements, the devnet chain ID,
input/output aliasing and overwrite of an existing output. The product
ChainSpec parser then requires the canonical `teeAttestationV1` field at every
startup. A hand-authored manifest cannot select `GramineDirectDev` outside
devnet chain ID `424242` or select `DcapRequired` outside testnet chain ID
`54322345`.

`scripts/prepare_network.py` exposes the same boundary for generated launch
plans:

```sh
python3 scripts/prepare_network.py \
  --seed /path/to/seed.json \
  --validators /path/to/validators.json \
  --output-dir /path/to/network \
  --tee-mode dcap-required \
  --chain-id 54322345 \
  --mrenclave 0x<exact-measurement> \
  --mrsigner 0x<exact-signer> \
  --isv-prod-id <id> \
  --minimum-isv-svn <svn> \
  --minimum-tcb-evaluation-data-number <number>
```

Do not use placeholder measurements. Do not start the testnet until the I9
release checkpoint records the exact signed artifacts and all required
hardware evidence.

## Verify identity and offer-key readiness

After a devnet bootstrap or an authorized testnet run, query each enclave and
compare its resident permanent key with canonical chain state:

```sh
target/debug/outbe-cli tee pubkey \
  --enclave-socket 127.0.0.1:17000 \
  --rpc-url http://127.0.0.1:18545 \
  --diff-chain
```

Before founding DKG, `recipient_x25519` is only the one-time onboarding
recipient and `offer_key_ready` is false. After founding finalization, the
resident permanent offer key is ready and must equal
`tributeOfferPublicKey()` on-chain. These values must never be confused.

Missing, corrupt, wrong-chain or otherwise unsealable permanent state is
terminal for that identity. There is no peer handoff, OperatorRecovery,
governance replacement, forced DKG or testnet-to-devnet fallback.

## Evidence labels

| Path | What it proves | What it does not prove |
| --- | --- | --- |
| `GramineDirectDev` | deterministic devnet behavior and reachable operator flow | SGX, DCAP, Intel collateral or testnet readiness |
| `mock-native` localnet | deterministic devnet behavior and reachable operator flow on a Gramine-less host | Gramine, the LibOS sandbox, SGX, DCAP or Intel collateral |
| private `#[cfg(test)]` verdict capability | I3-I8 state-machine behavior after the verifier boundary | quote parsing, QVL execution or hardware acceptance |
| synthetic cap vectors | deterministic bounds, allocation order and gas arithmetic | Intel-signed hardware evidence |
| fresh I9 `gramine-sgx` runs | exact-release Processor acceptance and timing | a future Platform node's own admission evidence |

See [Testnet SGX release and rollout](testnet-sgx-release.md) for the release
boundary and [Running a full node and a validator](becoming-a-validator.md) for
role-specific onboarding.
