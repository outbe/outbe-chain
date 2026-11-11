# SEV-SNP test fixtures

| File | Source |
|---|---|
| `milan-report.bin`, `milan-vcek.der` | test data of the `sev` crate 6.0.0 (virtee, Apache-2.0), `tests/certs_data` |
| `milan-ark.der`, `milan-ask.der` | AMD Milan root and intermediate certificates from the `sev` crate 6.0.0, `src/certs/snp/builtin/milan`, converted from PEM to DER |
| `simulator-report.bin`, `simulator-cert-table.bin`, `simulator-ark.der` | dstack simulator (`--simulated-tee dstack-amd-sev-snp`), commit `c88f60e781a62d542e0843b333f18a48d1458b88`; the mock certificates use ECDSA P-384, not RSA-PSS |
