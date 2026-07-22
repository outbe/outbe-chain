# Launching a TEE localnet with real SGX (gramine-sgx)

In this setup, tribute enclaves run under `gramine-sgx` on real SGX hardware with
confidential execution and EGETKEY sealing. Each enclave generates its own DKG
identity from the hardware RNG. The committee then converges on one tribute offer
key and publishes it on-chain. The e2e suite uses a different path:
`gramine-direct`, with mock SGX and a deterministic `--dkg-seed`.

> The manifest ships with `sgx.remote_attestation = "none"`. The enclaves still
> provide confidential execution, real MRENCLAVE/MRSIGNER measurements, and
> EGETKEY sealing, but they do not produce a DCAP quote. Quote generation requires
> a provisioned PCK through PCCS or Intel PCS, and
> `verify_enclave_registration` is still a stub, so the chain cannot verify quotes
> yet. See [Re-enabling DCAP](#re-enabling-dcap-attestation).

This page describes a developer-owned localnet. Its `Dockerfile.test` renders and
signs with a scenario-scoped test key at launch. It cannot authorize a testnet
deployment. Testnet operators must pull and verify the pre-signed image digest from
[Testnet SGX release and rollout](testnet-sgx-release.md); the release container has no
runtime signing or `gramine-direct` fallback.

## Prerequisites

- An SGX CPU with the in-kernel driver: `/dev/sgx_enclave` present.
- Docker access. The localnet harness builds the explicit
  `outbe-tee-enclave-gramine-test` image from `Dockerfile.test`; host Gramine is not
  used by that container path.
- The current user can reach SGX. Either be in the `sgx`/`sgx_prv` groups or run via
  `sudo`. If quote/AESM operations are ever needed: `sudo systemctl restart aesmd`.
- Docker, and the foundry tools (`cast`, on PATH) for the checks below.
- Build the binaries:
  ```sh
  cargo build -p outbe-chain --bin outbe-chain
  cargo build --release -p outbe-tee-enclave --bin outbe-tee-enclave   # the REAL enclave (no mock)
  cargo build -p outbe-cli --bin outbe-cli
  docker build -f bin/outbe-tee-enclave/gramine/Dockerfile.test \
    -t outbe-tee-enclave-gramine-test bin/outbe-tee-enclave/gramine
  ```

## Launch a four-validator SGX network

```sh
export PATH="$PATH:$HOME/.foundry/bin"
./scripts/bootstrap-testnet.sh 4 /tmp/sgx-net scripts/seed-testnet-lowstake.json

sudo env OUTBE_TEE_ENCLAVE=1 OUTBE_TEE_SEAL=1 \
  OUTBE_TEE_ENCLAVE_BINARY="$PWD/target/release/outbe-tee-enclave" \
  OUTBE_CHAIN_BINARY="$PWD/target/debug/outbe-chain" PATH="$PATH" \
  ./scripts/run-testnet.sh start /tmp/sgx-net
```

What the flags do:

- `OUTBE_TEE_ENCLAVE=1` without `OUTBE_TEE_ENCLAVE_MOCK` starts the real
  `outbe-tee-enclave` binary. `run-testnet.sh` passes `/dev/sgx_enclave` (and
  `/dev/sgx_provision` + the host AESM socket when present); the test entrypoint
  signs the mounted development binary with the localnet's generated test key and
  selects `gramine-sgx` because the SGX device is there.
- `OUTBE_TEE_SEAL=1` gives each validator a persistent `--tee-dir` (bind-mounted at
  `/tee`) and `--chain-id`. Under real SGX, it seals the DKG-derived offer key for
  faster restarts. It also makes the enclave generate its own DKG identity and seal
  it to `<tee-dir>/sealed_identity.bin`. No `--dkg-seed` is supplied, and the
  identity survives a container restart.
- The `gramine-direct` mock path (`OUTBE_TEE_ENCLAVE_MOCK=1`) keeps a deterministic
  per-index `--dkg-seed`: it has no EGETKEY and cannot persist a random identity.
- The generated test key lives under the localnet output directory with mode 0600,
  is reused only for that localnet's restart stability and must never be copied into
  a release or treated as the protected testnet signer.

Each enclave logs, on a healthy boot:

```
MODE = gramine-sgx — remote attestation DISABLED (...); EGETKEY sealing available, NOT remote-attested
listening on tcp://127.0.0.1:700i (attestation: none (gramine-sgx; ...); DKG identity: self-generated (fresh, sealed))
```

Wait for TEE bootstrap (real-SGX enclave load + DKG is slower than the mock):

```sh
cast call 0x000000000000000000000000000000000000EE0A 'isBootstrapped()(bool)' --rpc-url http://localhost:8545
# -> true
```

## Verify the keys

The `outbe-cli tee pubkey` command queries an enclave's **resident** offer key (the
Seam-F group key once DKG completes), its DKG identity (`tee_bls_pub`), and its real
measurements. With `--diff-chain` it also asserts the offer key equals the on-chain
registry `tributeOfferPublicKey()` and **exits non-zero on a mismatch**.

```sh
for i in 0 1 2 3; do
  ./target/debug/outbe-cli tee pubkey --enclave-socket 127.0.0.1:700$i \
    --rpc-url http://localhost:8545 --diff-chain
done
```

Expected results:

- **offer pubkey identical on all 4 + `✓ MATCH`** → the committee shares ONE offer
  key and it equals the on-chain registry. This is the key clients encrypt to.
- **`tee_bls_pub` DISTINCT on all 4** → identities were independently
  hardware-generated, not derived from a shared seed/index.
- **`mrenclave` real, non-zero, identical on all 4** → same signed binary; read from
  the local SGX report (no DCAP needed).
- **`remote-attested (real quote): false`** → confidential and measured, but not
  remote-attested (expected under `remote_attestation = "none"`).

Restart-stability of a self-generated identity:

```sh
sudo docker restart outbe-tee-gramine-0
# its tee_bls_pub is unchanged; the banner now says "self-generated (restored from seal)"
```

## Create a tribute against the registry offer key

`outbe-cli tribute offer` reads `tributeOfferPublicKey()` from the registry,
encrypts the offer to it, and submits `offerTribute(...)`; any committee enclave
decrypts it in-SGX during block execution.

```sh
V0=0x$(tr -d '[:space:]' < /tmp/sgx-net/validator-0/evm-key.hex)
ADDR=$(cast wallet address --private-key "$V0")

# Optional pre-flight: fail fast if the registry and enclave keys diverged.
./target/debug/outbe-cli tee pubkey --enclave-socket 127.0.0.1:7000 --rpc-url http://localhost:8545 --diff-chain

# The localnet boots on the CURRENT date (bootstrap-testnet.sh seeds the OFFERING
# day = genesis date, UTC+14), so offer for today, not a hardcoded calendar day:
WWD=$(date -u -d "@$(( $(date +%s) + 50400 ))" +%Y%m%d)
./target/debug/outbe-cli tribute offer "$WWD" --amount 100 --currency 840 \
  --private-key "$V0" --rpc-url http://localhost:8545

# After a block, the tribute is owned by the creator and supply is +1 on every node:
./target/debug/outbe-cli tribute by-owner "$ADDR" --rpc-url http://localhost:8545
cast call 0x0000000000000000000000000000000000001101 'totalSupply()(uint256)' --rpc-url http://localhost:8545
```

A registry/enclave key mismatch otherwise appears during execution as the opaque
error `AEAD decryption failed`. Run the `tee pubkey --diff-chain` pre-flight before
spending a transaction.

## The keys in the registry

- **Group offer key** — ONE per committee, registry slot-1 `tributeOfferPublicKey()`.
  Derived in-enclave from the DKG group threshold signature (Seam F), byte-identical
  on every honest enclave. This is what clients encrypt tributes to. `tee pubkey`'s
  `offer pubkey` (resident, post-DKG) is this value.
- **Per-enclave `recipient_x25519`** — a per-validator handoff key (registry slot for
  the validator), REPORT_DATA-bound, used to seal the offer key to a joining member.
  Distinct per enclave; not the key clients use.
- **DKG identity** (`tee_bls_pub` + share-decrypt key) — a third thing, per enclave,
  now self-generated under SGX.

## genesis teePolicy

The genesis `teePolicy` governs which enclave measurements the node will accept on
connect. An empty policy resolves to `dev_accept_any`, which accepts the
unattested/empty-quote enclaves produced under `remote_attestation = "none"`. On-chain,
`verify_enclave_registration` is a stub (accepts any measurements), so zero/any
measurements are currently accepted. Real measurement enforcement remains future work.

## Re-enabling DCAP attestation

To produce and verify real DCAP quotes:

1. Provision a PCK / quote provider (PCCS or Intel PCS) on the host.
2. For a local-only experiment, set `sgx.remote_attestation = "dcap"` in the test
   manifest template and rebuild `Dockerfile.test`. For an authorized release, change
   `outbe-tee-enclave.release.manifest.template`, review/version the bundle contract and
   create a new protected release; never modify or re-sign an existing image.
3. Tighten the genesis `teePolicy` to require the expected MRENCLAVE/MRSIGNER and a
   minimum ISV-SVN, and replace the `verify_enclave_registration` stub with real
   on-chain measurement enforcement.

Without PCK provisioning, `gramine-sgx` with `"dcap"` fails to load with
`AESM service returned error 12`. For that reason, this localnet ships with
`"none"`.
