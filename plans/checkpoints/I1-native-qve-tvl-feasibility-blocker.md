# I1 checkpoint — native verifier-boundary blocker (resolved)

Date: 2026-07-30

Audited tree: `9b1575f3a91c593b9458f3ab09a5c55f8aaba873`

Status: `RESOLVED` by the accepted enclave-resident native-QVL boundary.
This document preserves the blocker investigation as an audit trail. I1 may
continue without QvE/TVL; real SGX execution remains a fail-not-skip I9 release
gate.

## Accepted resolution

The selected option is **A: native QVL wholly inside the measured Gramine
enclave**.

Gramine's maintained DCAP integration confirms this is a supported
composition: `ra_tls_verify_dcap.so` calls `sgx_qv_verify_quote` in-process
with `p_qve_report_info = NULL`, while
`ra_tls_verify_dcap_gramine.c` supplies dummy URTS enclave-management symbols
instead of creating a nested Intel SDK enclave.

Outbe will use a smaller raw-QVL adapter rather than the stock RA-TLS wrapper:

- the only inputs are canonical evidence, active policy and consensus block
  timestamp;
- collateral is always non-null and submitted with the evidence;
- no QPL/PCCS, wall clock, environment, filesystem cache or host verdict is
  permitted during consensus;
- exact QVL and dependency artifacts are release-pinned and integrity-pinned
  in the signed Gramine manifest;
- the stable Outbe wrapper remains the only policy/verdict authority.

QvE and TVL are unnecessary because QVL and its result already execute inside
the attestation enclave. A complete migration to Intel SGX SDK and a second
verifier enclave are outside V1 scope.

## Superseded gate that exposed the blocker

The first I1 gate requires the exact-pinned native Intel QVL to run in QvE
mode over canonical evidence and the consensus block timestamp, followed by
`sgx_tvl_verify_qve_report_and_identity` inside the Outbe attestation enclave.
The gate must prove this composition in the current Outbe/Gramine runtime
before production implementation proceeds.

## Direct environment evidence

- Host architecture: `x86_64`, Intel Xeon E-2388G, CPU flags include `sgx` and
  `sgx_lc`.
- Gramine: `1.9`.
- Installed Intel runtime candidate:
  - `libsgx-dcap-quote-verify 1.26.100.1-noble1`;
  - `libsgx-ae-qve 1.26.100.1-noble1`;
  - `libsgx-dcap-ql 1.26.100.1-noble1`;
  - `libsgx-urts 2.29.100.1-noble1`.
- Relevant installed binary SHA-256 values:
  - QVL:
    `4745bc5b46cbdc17a78119ae2db08f54b86ff9077c5ab480f378741396365aef`;
  - signed QvE:
    `5773be9ee613b9f8065c1fbbf0f76a517cc148b043bcfc17b5337ae5d1ee1cc5`;
  - quote library:
    `b258f57a3dd459a6ba1745b5b9c8d5caa3950b913f9a394688088fac17be4923`;
  - URTS:
    `da6eec0d62a0b77fae5502239f5a24795f25e032a64733bf65f2ab30216fb7f3`.
- The CPU supports SGX, but the kernel SGX module and `/dev/sgx_enclave`,
  `/dev/sgx_provision`, `/dev/sgx/*` and `/dev/isgx` are absent.
- QVL development headers
  (`libsgx-dcap-quote-verify-dev 1.26.100.1-noble1`) and
  `libsgx-headers 2.29.100.1-noble1` are now installed. The Intel SGX SDK and
  `libsgx_dcap_tvl.a` are not installed and are not required by the accepted
  boundary.
- The current Outbe release manifest sets
  `sgx.remote_attestation = "none"`. Its release contract contains no pinned
  QVL, QvE, TVL, URTS or Intel trusted-runtime artifact set.

The missing SGX device prevented the former QvE/TVL proof. Under the accepted
boundary it no longer blocks the I1 offline deterministic-verification work;
it remains relevant to the mandatory I9 hardware release gate.

## Why the Secret Network composition does not fit directly

Secret Network uses two distinct Intel SGX SDK sides:

1. Its untrusted host OCALL invokes `sgx_qv_verify_quote` with a non-null QvE
   report-info structure and explicit collateral.
2. Its application enclave is built with Intel SGX SDK/edger8r and statically
   links `libsgx_dcap_tvl`, `libsgx_trts`, trusted libc, crypto and service
   libraries.
3. The enclave calls `sgx_tvl_verify_qve_report_and_identity`, authenticating
   the QvE local report, nonce and exact quote/result/supplemental binding.

Evidence:

- `/home/ubuntu/SecretNetwork/cosmwasm/packages/sgx-vm/src/attestation_dcap.rs`;
- `/home/ubuntu/SecretNetwork/cosmwasm/enclaves/shared/crypto/src/dcap.rs`;
- `/home/ubuntu/SecretNetwork/cosmwasm/enclaves/execute/Enclave.edl`;
- `/home/ubuntu/SecretNetwork/cosmwasm/enclaves/execute/Makefile`.

Outbe's enclave is instead a normal Linux ELF running under the Gramine LibOS.
Gramine exposes local report and quote generation through `/dev/attestation`,
but no supported application API for authenticating an incoming SGX report.
Its internal `sgx_verify_report` is not exported to the application. Intel TVL
depends on that primitive plus Intel trusted-runtime memory, crypto and libc
APIs. Linking Intel TRTS into the existing Gramine ELF would combine
incompatible enclave runtimes and is not a supported composition.

The installed Gramine `libra_tls_verify_dcap_gramine.so` does not close this
gap. It links native QVL and performs QVL-only verification; it does not call
Intel TVL or authenticate a QvE report. Its stock path also supplies local
wall time and permits QPL collateral lookup, which are forbidden consensus
inputs.

References:

- `https://gramine.readthedocs.io/en/v1.9/attestation.html`;
- `https://github.com/intel/confidential-computing.tee.dcap/tree/main/SampleCode/QuoteVerificationSample`.

## Three-agent blocker review

Three independent reviews examined the current Outbe runtime, Intel
QVL/QvE/TVL ABI and Secret Network:

- all found the direct Secret Network-to-Gramine TVL linkage unsupported;
- all rejected trusting a host QVL result without enclave authentication;
- all found that a retained quote/collateral fixture cannot replace a live
  QvE/TVL proof because the QvE report targets the local verifier enclave;
- all required a real SGX host with exposed devices for the executable gate.

No agent changed repository files.

## Decision options

### A. Native QVL wholly inside the measured Gramine enclave

Package the exact-pinned QVL and dependencies as measured Gramine trusted
files. Invoke QVL in its in-process verification mode with non-null canonical
collateral and the consensus block timestamp. Disable QPL/PCCS substitution
and map results through the existing strict Outbe wrapper.

The host supplies bytes but has no verdict authority because QVL itself runs
inside the measured enclave. QvE and TVL become unnecessary. This is the
smallest option compatible with the current enclave runtime, but it changes
the accepted canonical boundary and therefore requires approval.

### B. Add a dedicated Intel SGX SDK verifier enclave

Keep host QVL in QvE mode and put exact Intel TVL in a small separate Outbe
SDK enclave. This follows Intel's supported composition, but requires a new
measurement, nonce state and authenticated verdict channel between the SDK
enclave and the Gramine/node boundary. Those mechanisms are not in the
current plan and would expand the architecture.

### C. Add a pinned Gramine fork/TVL compatibility layer

Expose incoming-report verification and make Intel TVL work against Gramine.
This creates a security-sensitive runtime fork and an unproven compatibility
surface. It is the highest-risk option.

### D. Return to the pure-Rust consensus verifier

This would reopen the superseded `dcap-qvl 0.5.2` design and is currently an
explicit non-goal.

Host QVL without TVL, QvE authentication or an enclave-resident QVL is not an
option because a host can forge the result.

## Decision record

The user accepted option A on 2026-07-30. I1 resumes with the updated
feasibility gate recorded in the canonical decision map, engineering-gate
evidence and implementation plan. Access to an SGX host with
`/dev/sgx_enclave` and `/dev/sgx_provision`, or a designated SGX CI runner, is
still required before I9 can pass.

The follow-up executable proof is recorded in
`plans/checkpoints/I1-native-qvl-feasibility.md`.
