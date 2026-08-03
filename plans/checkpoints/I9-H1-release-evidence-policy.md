# I9 H1 checkpoint — release evidence policy

Date: 2026-08-03

Status: `DECISION ACCEPTED`; H1 closure remains pending.

## Decision

Outbe follows the deployed Secret Network security boundary for Platform PCK
CA support: every real node is admitted only after its own Intel-rooted quote,
canonical collateral, consensus time, TCB status, identity and measurement pass
the testnet enclave-resident verifier. A failed Platform admission receives
no offer key and cannot join consensus or process transactions.

The release workflow therefore requires one fresh accepted Processor-CA run for
the exact signed release. It does not require a dedicated registered
multi-package Platform runner on every release. Synthetic or self-signed
Platform vectors remain parser/policy evidence only and can never satisfy a
real node admission.

## Operational boundary

- `testnet-release-sgx` remains a fail-not-skip Processor/SGX runner.
- `ReleaseManifest` requires `fresh-accepted-processor-dcap` and no Platform
  release input.
- `outbe-release-dcap-evidence --expected-pck-ca platform` remains available
  for on-demand compatibility diagnosis and real-node evidence capture.
- Guest-visible socket count is untrusted provenance. The enclave-verified
  issuer in the Intel-signed type-5 PCK chain is the CA authority.
- QPL/QCNL remains host-only acquisition machinery and never supplies a
  consensus verdict.

## H1 work still required

This decision does not make H1 pass. H1 still needs:

- the final release commit and signed immutable tag identity;
- the protected signed exact-release OCI and measurements;
- a fresh accepted Processor-CA quote with current Processor/root collateral;
- exact testnet chain/genesis/active-policy binding;
- retained canonical evidence from the public Begin/Chunk/Finish verifier.

The release evidence harness now uses the canonical testnet chain ID
`54322345`, but still derives a synthetic genesis hash and a measurement-derived
initial policy. That evidence is useful for hardware-path qualification but
cannot authorize a testnet release. Before H1 can pass, the harness must consume
the canonical testnet genesis and exact active policy bytes, and the finalizer
must verify their hashes against the evidence. Until then the protected
workflow is ineligible for testnet activation.

The current rented Processor host previously returned
`ConfigurationAndSWHardeningNeeded`, which is outside the accepted
`UpToDate | SWHardeningNeeded` Platform status policy. A new capture may be
attempted only after its platform configuration changes or on another eligible
Processor host; the policy is not weakened to admit it.

Git push, PR mutation, governance action and Beads/Dolt sync: `false`.
