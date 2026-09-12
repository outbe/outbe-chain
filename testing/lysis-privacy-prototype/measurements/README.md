# Reproduce the private-flow component measurements

Historical component experiments. The current requirements and research status are indexed in [the parent README](../README.md). These saved timings do not establish the current full P_link memory limit or end-to-end private-flow throughput.

Measured on 2026-09-11, Apple M4 Max, 14 CPU cores (10 performance / 4 efficiency), 36 GB RAM, macOS arm64. Release builds, single-process sequential benchmarks, CPU not isolated. See [the target trace](../TARGET_TRACE_NO_TEE.md) for protocol meaning and missing integration.

New components are documented separately: [P-384 wide-field VSS](wide-vss/README.md) opens one full daily sum; [P_link](p-link/README.md) proves canonical L2 draft hash → exact nominal → the same P-384 commitment and includes a saved-proof/VSS/reshare composition check. The older Ristretto/SEAL measurements below retain their original scopes.

## What the binaries actually do

> Precision update: the target now uses nominal/fraction/entry-price inputs at 10^6 and exact Gratis/cost/balance results at 10^18. The saved claim/withdrawal timings below still measure the earlier floor-based, 10^6-balance variant. They have not been rerun or relabeled as exact-flow benchmarks. See section 0 of the target trace.

- `src/main.rs`: Pedersen commitments on Ristretto; custom Bulletproofs R1CS integer arithmetic over four 64-bit limbs per uint256; Pedersen VSS (16/11 and 128/86); one mathematical 16/11 → 16/11 handoff; linked issue → aggregate → claim → withdrawal fixture.
- `src/bin/worker_costs.rs`: includes the repository's actual `lysis/src/algorithm.rs` coefficient kernel with minimal error/constants shims; measures a proposed 343-byte Nod record encoding and one Keccak per record. It does not run an OCOMP worker or production Nod serialization.
- `seal_bench.cpp`: Microsoft SEAL 4.4.0 BFV, degrees 4096 and 8192, 128-bit parameter security setting, 50-bit batching modulus, sixteen 16-bit slots per uint256 input. One process holds the full secret key.
- `scale_model.py`: derives traffic, state sizes and component CPU estimates from the JSON files. It is not a network load test.

**Research only.** Bulletproofs 5.0.0 R1CS is enabled through its experimental `yoloproofs` feature. The custom arithmetic gadgets are unaudited. The Rust fixture uses a **fixed random seed for repeatability** and logs selected private fixture values as debug data; it must never be used as a wallet/protocol implementation or as evidence that those fixture secrets are private.

The VSS fixture opens four aggregate limb sums. This gives exact integer reconstruction, but reveals more aggregate information than just the final S. A strictly only-S full-uint256 opening requires an additional protocol described in T03 of the trace. The separate wide-vss component now measures that opening; it is not implemented by this older executable.

## Rust reproduction

The crate is intentionally isolated from the production workspace. All dependencies used on this machine were already cached. Keep `Cargo.lock`.

From the repository root:

~~~sh
python3 testing/lysis-privacy-prototype/measurements/prepare_vendor.py
cargo run --offline --locked --release \
  --manifest-path testing/lysis-privacy-prototype/measurements/Cargo.toml \
  --bin outbe-private-flow-measurements \
  > testing/lysis-privacy-prototype/measurements/rust_results.json
cargo run --offline --locked --release \
  --manifest-path testing/lysis-privacy-prototype/measurements/Cargo.toml \
  --bin worker_costs \
  > testing/lysis-privacy-prototype/measurements/worker_results.json
~~~

If Bulletproofs 5.0.0 source is not in the Cargo registry cache, obtain that exact crate source and pass `prepare_vendor.py --source /path/to/bulletproofs-5.0.0`. On a machine with uncached dependencies, the initial Cargo dependency download requires network access and removal of `--offline`.

The preparation script copies the cached package into ignored `vendor/bulletproofs`. It applies exactly three decoding compatibility changes in `src/r1cs/proof.rs`: convert `CtOption<Scalar>` into `Option<Scalar>` before `.ok_or(...)`. The registry copy is never edited. [vendor_provenance.json](vendor_provenance.json) records the original/patched decoder SHA-256. This compatibility patch does not constitute an audit of the experimental backend.

The arithmetic benchmark uses one warmup and four retained samples; reported median is the average of the two central values. Proof generation includes commitment creation and circuit/proof construction, but excludes common Bulletproof generator construction. VSS uses eight samples. Coefficient/encoding timing uses eight batches. SEAL encrypt/decrypt uses two warmups and thirty samples; the SEAL helper reports the upper central sample. The difference in median convention is negligible compared with the unmeasured protocol work, but is stated for reproduction.

Proof scopes:

| Operation | Proven in this prototype | Not proven here |
|---|---|---|
| Issue | uint256 ranges, positive amounts, exact floor from public p/d, public-context binding | Offer authenticity, source signature/membership/nullifier, real admission |
| Claim | Exact floors for g and cost, positive g/c, balance addition, range, context | Private PayNote authorization/spend, Nod eligibility/PoW/spent state, Fidelity update |
| Withdrawal | Private old/new balance relation to public x, range, context, native amount overflow guard | Chain authorization/replay state, actual burn/native mint, Fidelity update |

Four 64-bit limbs avoid treating uint256 as a scalar modulo the Ristretto group order. Integer relations use bounded carries and remainder constraints. Measured issue p=d=10^12 is a specific public-price profile; arbitrary wider public prices can increase circuit size. The public context in the fixture is a synthetic transcript, not a production message schema.

The arithmetic relation is not a full byte-for-byte port of the current source parser or every intermediate-overflow rule in the enclave. In particular, it does not implement the current wire parser's u64 whole-amount restriction or prove the enclave's U512 intermediate-overflow rejection for all possible public prices.

Negative checks executed: wrong nonce, wrong claim coefficient, overcredited balance, incorrect floor output, and bad VSS share. uint256 MAX is accepted for the identity issue relation and zero nominal is rejected.

VSS verifies coefficient commitments and recipient shares. The handoff selects fixed qualified coordinates 1..t and checks every old-dealer/new-recipient pair locally. There is no distributed qualified-set agreement, Byzantine network/liveness experiment, authenticated transport, erasure verification, or mobile-adversary test.

## SEAL reproduction

The recorded build was installed at `/private/tmp/outbe-lysis-seal-install` from SEAL 4.4.0. The benchmark accepts another installation prefix:

~~~sh
sh testing/lysis-privacy-prototype/measurements/run_seal.sh \
  /private/tmp/outbe-lysis-seal-install
python3 testing/lysis-privacy-prototype/measurements/scale_model.py
~~~

For a fresh build, use the official [SEAL 4.4.0 source](https://github.com/microsoft/SEAL/tree/v4.4.0), C++17, static library, and disable zlib/zstd compression to match the recorded configuration. `run_seal.sh` assumes the default installation layout with `include/SEAL-4.4` and `lib/libseal-4.4.a`.

The recorded `Serialization::compr_mode_default` is uncompressed; its byte count equals the explicitly uncompressed count. Encryption uses the public key, so the result does not use secret-key seeded ciphertext savings.

The billion-count stress doubles/adds the SAME ciphertext in O(log N) additions. It is not a billion-message throughput test, and it is not a parameter proof for arbitrary malicious ciphertexts. Degree 4096 fails this stress; degree 8192 produces the exact sixteen summed limbs. Neither profile implements threshold decryption, proof of a valid input/equality to Tribute commitments, private regrouping, or key rotation.

## Results and provenance

- [rust_results.json](rust_results.json): measured proof/VSS timings, proof lengths, checked fixture.
- [worker_results.json](worker_results.json): actual coefficient kernel and proposed encoding component timings.
- [seal_results.json](seal_results.json): BFV parameters, sizes, arithmetic/noise stress.
- [scale_results.json](scale_results.json): derived million/billion model, not measured capacity.
- [provenance.json](provenance.json): hardware, versions, revision and source/result hashes.

Run the binaries sequentially when comparing CPU timings. Do not sum local CPU timings and present the result as consensus latency. The final production transaction sizes remain unknown until source, PayNote, Fidelity and availability proofs have concrete encodings.
