# Mainnet SGX/DCAP release and rollout

Outbe Mainnet uses the canonical identity `676 / outbe-mainnet-1`. It runs the same
production protocol defaults and enclave bytes as Testnet; the release profile differs in
the identities that must never be transferable between networks: chain ID, chain name,
genesis, OCI repository, protected signing scope, Sigstore workflow identity, and DCAP
archive genesis member.

No private key or placeholder Mainnet genesis belongs in the repository. The exact final
genesis is produced from the explicit Mainnet configuration and canonical production
defaults. Its hash is computed from those bytes. The protected workflow requires an HTTPS
URL and lowercase SHA-256 for those exact bytes and rejects any genesis whose `chainId` is
not `676`.

## Protected environment

Create the GitHub Environment `mainnet-release`, require reviewers, and configure:

- secret `MAINNET_SGX_SIGNING_KEY_B64` containing the protected SGX signing key;
- variable `MAINNET_GENESIS_URL` pointing to the immutable exact `mainnet-genesis.json`;
- variable `MAINNET_GENESIS_SHA256` containing its 64-character lowercase SHA-256;
- a self-hosted runner labelled `mainnet-release-sgx` with the pinned QPL and SGX devices.

The key is decoded only inside the protected signing job, mounted read-only at the generic
container secret path, removed by an exit trap, and never uploaded. Do not commit signing
keys or reuse the Testnet key as Mainnet authority.

Configure immutable signed tags matching `vX.Y.Z-mainnet.N`. Pushing such a tag makes
`release.yml` exclude it from GoReleaser and dispatch `mainnet-release.yml` from
`refs/heads/main`. The dispatcher reads the exact genesis URL/hash from the protected
environment. Every privileged job rechecks the signed tag object and commit before use.

## Network-bound release path

The Mainnet workflow performs the same reproducible build, SGX signing, OCI publication,
hardware SGX restart/sealing test, and enclave-resident DCAP verification as Testnet, but
passes `--network mainnet` at every typed boundary. It retains `mainnet-genesis.json` in
the DCAP archive and ReleaseManifest; archives containing `testnet-genesis.json`, both
genesis names, or neither are rejected.

Local diagnosis uses the same typed commands with an explicit profile:

```bash
mise run release-sgx-prepare -- \
  --network mainnet \
  --elf-output /tmp/outbe-elf-a \
  --output /tmp/outbe-mainnet-sgx-a

mise run release-sgx-sign -- \
  --network mainnet \
  --unsigned /tmp/outbe-mainnet-sgx-a \
  --key-file /secure/mainnet-sgx-key.pem \
  --output /tmp/outbe-mainnet-sgx-signed

mise run release-sgx-verify -- \
  --network mainnet \
  --bundle /tmp/outbe-mainnet-sgx-signed
```

Hardware acceptance requires an already Cosign-verified image digest and the exact genesis:

```bash
cargo run --locked -p outbe-e2e-harness --bin outbe-release-sgx-e2e -- \
  --network mainnet \
  --image 'ghcr.io/outbe/outbe-tee-enclave-mainnet@sha256:<64-hex-digest>' \
  --bundle /tmp/outbe-mainnet-sgx-signed \
  --evidence /tmp/mainnet-hardware-sgx.json

cargo run --locked -p outbe-e2e-harness --bin outbe-release-dcap-evidence -- \
  --network mainnet \
  --image 'ghcr.io/outbe/outbe-tee-enclave-mainnet@sha256:<64-hex-digest>' \
  --bundle /tmp/outbe-mainnet-sgx-signed \
  --genesis /secure/mainnet-genesis.json \
  --expected-pck-ca processor \
  --output-dir /tmp/mainnet-hardware-dcap-processor
```

These commands are release mechanics, not a launch ceremony. Validator keys, balances,
the explicit validator set, and the final genesis bytes remain operator inputs. Devnet and
Testnet shortcuts are not enabled for chain ID 676.
