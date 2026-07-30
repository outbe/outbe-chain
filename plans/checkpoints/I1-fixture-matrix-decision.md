# I1 fixture-matrix decision

Date: 2026-07-30

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

- I1 requires a real Processor-CA positive/negative cryptographic corpus whose
  quote is generated from `RegistrationIntentV1::report_data()`.
- I1 uses official Intel synthetic Platform-CA vectors for exact grammar, CA
  classification, Platform/QE policy and stable rejection coverage.
- Synthetic Platform evidence is labelled as such and never counted as a
  native-QVL positive or real-hardware result.
- I9 requires a real Intel-rooted Platform-CA quote and collateral captured on
  a registered multi-package SGX platform. Missing hardware or evidence fails
  the release job; it is never skipped.

This changes fixture staging, not the production verifier or admission policy.
Production still admits both Processor and Platform PCK CA chains only after
the same native Intel QVL, canonical collateral, time, status, identity and
measurement checks succeed.

## Test boundary

Synthetic inputs are permitted for quote/collateral grammar, caps, trailing
bytes, CA classification, exhaustive status mapping, gas overflow and stable
reject ordering. Positive cryptographic verification, exact intent binding,
Intel root/FMSPC/PCE identity and final stable verdict require real signed
Processor evidence in I1 and real signed Platform evidence in I9.
