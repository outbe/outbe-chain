# Launching TEE networks after the DCAP activation boundary

Outbe has two explicit, genesis-fixed TEE modes. They are different networks,
not runtime alternatives:

- `DcapRequired` is the production Intel SGX x86_64 mode. It requires the V1
  manifest from block 1, an authorized SGX enclave, quote and canonical
  collateral. A missing or rejected dependency stops startup; it never falls
  back to development mode.
- `GramineDirectDev` is an isolated development mode with reserved chain ID
  `54322345`, its own genesis and non-hardware measurements. It runs under
  `gramine-direct` and cannot be used as SGX, DCAP or release evidence.

Every node role requires `--tee-enclave-socket`. A Validator cannot start
threshold work or consensus without the permanent resident offer key, except
while a proven fresh block-1 founder is creating that key. A FullNode must have
the exact permanent key and match it against its selected certified upstream
before Reth networking, RPC, sync or execution launches.

## Current release status

The A0 code path makes `teeAttestationV1` and OST3 mandatory and fail-closed.
That alone is not a production release claim. Production rollout remains
blocked until the remaining I9 gates prove all of the following for one exact
artifact set:

- Intel QVL `1.26.100.1-noble1`, Gramine `1.9`, the `native-dcap` feature and
  every trusted native artifact are frozen and digest-verified;
- fresh accepted Processor-CA and registered multi-package Platform-CA evidence
  passes the enclave-resident public verifier;
- exact-release `gramine-sgx` and dense block-1 timing fit the published
  minimum validator profile;
- real Validator, FullNode and 32-validator `DcapRequired` E2E is green.

The checked-in B1 candidate now declares `sgx.remote_attestation = "dcap"` and
builds the enclave with exactly `native-dcap`. It still must not be deployed as
a `DcapRequired` production release until B1 binds the reproducible artifact
set; H1, P1 and E1 own hardware acceptance, performance and production E2E.

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

## Construct a production ChainSpec

Use only measurements and TCB values taken from the exact frozen release. The
command below validates and writes a ChainSpec; it does not prove that the
release passed hardware gates:

```sh
target/debug/outbe-chain tee genesis \
  --input /path/to/final-seeded-genesis.json \
  --output /path/to/production-genesis.json \
  --mode dcap-required \
  --mrenclave 0x<exact-32-byte-release-measurement> \
  --mrsigner 0x<exact-32-byte-release-signer> \
  --isv-prod-id <exact-release-product-id> \
  --minimum-isv-svn <reviewed-minimum-svn> \
  --minimum-tcb-evaluation-data-number <reviewed-nonzero-tcb-number>
```

The command refuses zero measurements, the reserved development chain ID,
input/output aliasing and overwrite of an existing output. The product
ChainSpec parser then requires the canonical `teeAttestationV1` field at every
startup. A hand-authored manifest cannot select `GramineDirectDev` outside
chain ID `54322345` or select `DcapRequired` on that reserved identity.

`scripts/prepare_network.py` exposes the same boundary for generated launch
plans:

```sh
python3 scripts/prepare_network.py \
  --seed /path/to/seed.json \
  --validators /path/to/validators.json \
  --output-dir /path/to/network \
  --tee-mode dcap-required \
  --chain-id <production-chain-id> \
  --mrenclave 0x<exact-measurement> \
  --mrsigner 0x<exact-signer> \
  --isv-prod-id <id> \
  --minimum-isv-svn <svn> \
  --minimum-tcb-evaluation-data-number <number>
```

Do not use placeholder measurements. Do not start production until the I9
release checkpoint records the exact signed artifacts and all required hardware
evidence.

## Verify identity and offer-key readiness

After a development bootstrap or an authorized production run, query each
enclave and compare its resident permanent key with canonical chain state:

```sh
target/debug/outbe-cli tee pubkey \
  --enclave-socket 127.0.0.1:7000 \
  --rpc-url http://127.0.0.1:8545 \
  --diff-chain
```

Before founding DKG, `recipient_x25519` is only the one-time onboarding
recipient and `offer_key_ready` is false. After founding finalization, the
resident permanent offer key is ready and must equal
`tributeOfferPublicKey()` on-chain. These values must never be confused.

Missing, corrupt, wrong-chain or otherwise unsealable permanent state is
terminal for that identity. There is no peer handoff, OperatorRecovery,
governance replacement, forced DKG or production-to-development fallback.

## Evidence labels

| Path | What it proves | What it does not prove |
| --- | --- | --- |
| `GramineDirectDev` | deterministic development behavior and reachable operator flow | SGX, DCAP, Intel collateral or production readiness |
| private `#[cfg(test)]` verdict capability | I3-I8 state-machine behavior after the verifier boundary | quote parsing, QVL execution or hardware acceptance |
| synthetic cap vectors | deterministic bounds, allocation order and gas arithmetic | Intel-signed hardware evidence |
| fresh I9 `gramine-sgx` runs | exact-release Processor/Platform acceptance and timing | nothing beyond the recorded artifact and host identity |

See [Testnet SGX release and rollout](testnet-sgx-release.md) for the release
boundary and [Running a full node and a validator](becoming-a-validator.md) for
role-specific onboarding.
