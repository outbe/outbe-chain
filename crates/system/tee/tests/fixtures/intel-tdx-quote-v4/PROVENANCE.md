# Intel TDX quote v4 fixture

A real Intel TDX quote of version 4 and the Intel collateral that was current
when the quote was taken.

| File | Content |
|---|---|
| `tdx-quote-v4.bin` | TDX quote, 5006 bytes |
| `tdx-collateral-wrapper.json` | PCK CRL and its issuer chain, root CA CRL, TDX TCB info, TD QE identity |

Source: the `sample/tdx_quote` and `sample/tdx_quote_collateral.json` files of
the `dcap-qvl` crate, version 0.7.0 (MIT license).

The collateral is valid from 2025-06-19T10:32:27Z to 2025-07-19T10:16:03Z. The
tests use a fixed consensus time inside this window.

The report data of the quote does not bind an Outbe registration intent. The
public verifier therefore stops at the report data check. The tests that need
an accepted quote call the verification steps after that check directly.

The quote file has 70 zero bytes after the signature data. A canonical quote
ends with its signature data, so the tests cut the file to 4936 bytes.
