# Intel Platform-CA parser vector

This directory contains one official Intel PCK leaf-certificate parser vector.
It is synthetic/parser evidence only and must never be reported as a real SGX
quote, native-QVL success or hardware acceptance result.

Source:

- repository: `intel/SGX-TDX-DCAP-QuoteVerificationLibrary`
- commit: `89c589f24ce11f74271a0cd2e2d75182b5876dd8`
- path:
  `Src/AttestationParsers/test/IntegrationTests/ParseX509CertificateIT.cpp`
- upstream symbol: `PlatformPEM`
- upstream purpose: Platform-CA / scalable-platform PCK parser integration test
- upstream license: Apache-2.0
- local PEM SHA-256:
  `fe3cf9e9c337ddf9da3c7c2682befb4a4d707de2fe82641bcbc512718a64c644`

The vector has no associated Intel-rooted type-5 quote or collateral bundle.
I1 uses it only to prove deterministic Platform-CA, FMSPC and PCE-ID parsing.
The real Platform-CA native-QVL and end-to-end gate remains mandatory in I9.
