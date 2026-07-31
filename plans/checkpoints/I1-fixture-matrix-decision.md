# I1 fixture-matrix decision

Date: 2026-07-30

Amended: 2026-07-31

Status: accepted scope amendment; implementation may resume

## Blocker

The repository's only real SGX Quote v3 fixture is Processor-CA, carries
`REPORT_DATA = "Hello, world!" || zeroes`, and produces native-QVL Platform
status `ConfigurationAndSWHardeningNeeded`. It is a useful cryptographic
negative vector, but it cannot be changed into a positive Outbe intent-bound
fixture without invalidating its signatures.

No real Intel-rooted type-5 Platform-CA quote with its complete collateral
bundle was found in Secret Network or the inspected official Intel
repositories. Intel's public Platform-CA coverage consists of a real PCK
certificate parser vector and synthetic verification vectors. A real
Platform-CA quote requires a registered multi-package SGX platform; a
single-package Processor-CA host cannot generate one.

## Decision

The owner selected the minimal split:

- I1 requires a real Processor-CA cryptographic corpus generated from
  `RegistrationIntentV1::report_data()`, replayed through the public verifier
  and pinned QVL with its authentic strict-policy result preserved.
- I1 uses official Intel synthetic Platform-CA vectors for exact grammar, CA
  classification, Platform/QE policy and stable rejection coverage.
- Synthetic Platform evidence is labelled as such and never counted as a
  native-QVL positive or real-hardware result.
- I9 requires a fresh accepted Processor-CA capture for the exact release
  enclave/policy and a real Intel-rooted Platform-CA quote and collateral from
  a registered multi-package SGX platform. Missing hardware or either accepted
  result fails the release job; it is never skipped.
- Ordinary CI replays immutable real evidence at a fixed historical consensus
  timestamp without SGX hardware or live PCS/PCCS; live capture is not repeated
  per test case.

This changes fixture staging, not the production verifier or admission policy.
Production still admits both Processor and Platform PCK CA chains only after
the same native Intel QVL, canonical collateral, time, status, identity and
measurement checks succeed.

## Test boundary

Synthetic inputs are permitted for quote/collateral grammar, caps, trailing
bytes, CA classification, exhaustive status mapping, gas overflow and stable
reject ordering. Positive cryptographic verification, exact intent binding,
Intel root/FMSPC/PCE identity and the authentic strict-policy result require
real signed Processor evidence in I1. An accepted Processor verdict and real
signed Platform evidence remain mandatory in I9; synthetic or fake-verifier
results cannot satisfy either release gate.
