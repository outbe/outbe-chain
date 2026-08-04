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

The release evidence harness and finalizer now require the same tracked
`release/testnet-genesis.json` from the immutable testnet commit. Both parse it
through the node's fail-closed ChainSpec path. The harness retains the exact
genesis, block-1 policy and policy-schedule bytes; the finalizer independently
checks their hashes, the Validator and FullNode measurement rules, and every
member of the canonical Processor evidence archive. The signed
`ReleaseManifest` Processor gate binds both the canonical summary and the full
archive.

`release/testnet-genesis.json` is now a structurally valid default template for
testnet chain ID `54322345` and block-1 `DcapRequired`. It deliberately pins
non-release placeholder measurements (`MRENCLAVE = 0x11…11`, `MRSIGNER =
0x22…22`, `ISVPRODID = 65535`, minimum `ISVSVN = 65535`) and therefore cannot
authorize the current signed release. The release finalizer still requires an
exact match with the signed enclave measurements.

H1 remains pending. Before testnet, operators must finalize the complete seeded
genesis (including allocations and the other startup contracts), then regenerate
`teeAttestationV1` from that seed and the exact signed release measurements.
Editing header-affecting genesis fields or measurement bytes in place is invalid:
the genesis hash, canonical policy bytes, policy hash and schedule hash must be
regenerated together.

The current rented Processor host returns
`ConfigurationAndSWHardeningNeeded`, which is accepted by the explicit testnet
Platform policy together with `UpToDate` and `SWHardeningNeeded`. Its Intel
advisory IDs remain visible in the stable verdict and QE still must be exactly
`UpToDate`. The host is therefore eligible for the fresh exact-release capture
and, on that same SGX server, the QVL and full-block budget measurements. The
historical accepted replay does not satisfy the exact-release freshness gate.

Git push, PR mutation, governance action and Beads/Dolt sync: `false`.
