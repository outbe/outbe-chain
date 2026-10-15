# Superseded durable-store candidate

Runtime source `4d74923f7ee8d35a59124841d956a24d5feb2514` passed its original
53 adapter tests, seven consumer checks, Clippy/fmt (checks.json), but a later
fault regression rejected this candidate. No task acceptance was claimed.

`corrupt-witness-red.json` records exact runtime base, test-only patch hash,
argv/cwd/env/toolchain/platform, times, exit101 and log hash. Applying the recorded
patch to this runtime reproduces the failed native scenario. The cache's originally
selected witness is corrupted after another valid subset is already stored. The
old method returned a new Selected token without checking the witness retained by
the durable selection pointer; reopening could not load that selection.

Corrected source `2ddfe4a5382511f83d4ba4ae2f31a7a0b4ce591a` validates the already
selected package before issuing another token. See sibling review-store-r2 for
the extended full native test and frozen all-green checks. The old checks/logs and
failure remain evidence, not a current PASS or a complete P3/P6 implementation.
