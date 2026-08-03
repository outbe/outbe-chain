# Intel Platform-CA parser vector

This directory contains one official Intel PCK leaf-certificate parser vector.
It is synthetic/parser evidence only and must never be reported as a real SGX
quote, native-QVL success or hardware acceptance result.

Source:

- repository: `intel/SGX-TDX-DCAP-QuoteVerificationLibrary`
- tag: `v1.26`
- commit: `f3a6f2ecb49c13b7092f143f11675258914eff23`
- path:
  `Src/AttestationParsers/test/IntegrationTests/ParseX509CertificateIT.cpp`
- upstream symbol: `PlatformPEM`
- upstream purpose: Platform-CA / scalable-platform PCK parser integration test
- upstream file license: BSD-3-Clause
- upstream notice and terms: [LICENSE.intel](LICENSE.intel)
- local PEM SHA-256:
  `fe3cf9e9c337ddf9da3c7c2682befb4a4d707de2fe82641bcbc512718a64c644`

The vector has no associated Intel-rooted type-5 quote or collateral bundle.
I1 uses it only to prove deterministic Platform-CA, FMSPC and PCE-ID parsing.
Intel v1.26 `VerifyQuoteIT.cpp` separately generates an ephemeral self-signed
Platform-CA chain and quote for Intel's own verification-library tests; it is
test topology, not a reusable Intel-rooted evidence fixture, and Outbe does not
feed it to the public verifier.

Outbe's private Platform/QE policy matrices start from the raw SGX result values
in exact-pinned `libsgx-headers 2.29.100.1-noble1`, paired with QVL
`1.26.100.1-noble1`; they are not derived from this certificate.
A real Platform node must still pass native QVL and end-to-end admission before
it receives an offer key or joins consensus; this parser vector cannot satisfy
that node-specific gate.
