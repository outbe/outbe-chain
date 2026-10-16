# P2 actual fresh managed receiver rejects missing publication provider

Frozen source `0dbd83c40ace71773a0e358809bd2f1876c18f37`, base `53f369ff532b96e6fe686993f78ca3894e797bf9`. Test-only extension; production code unchanged from base.

The actual TCP/UDS native BLS peer scenario now includes a fresh guarded receiver. Before its repository exists, trusted local configuration installs the managed marker for the exact RID and sole BLS owner. Runtime reopens Storage, so the provider is unavailable in the new runtime instance; the persistent marker must prevent permissive native fallback. Actual fetch reaches identity verification, fails with the expected rad/id verification error, and creates no final canonical repository path. The same source identity/Ed patch/native BLS Review/signed refs are accepted by the ordinary static receiver in the same test. BLS actor, operation IDs and transport IDs plus the guarded rejection and absent canonical path are in the actual runtime logs. Owner Ed substitution is not introduced.

This tests missing-provider fail-closed behavior after runtime reopen, not a supplied provider authorizing a finalized network receipt. It is not an old-share/Vote exploit claim. It complements the direct/current-entry managed positive fixture in ../local-publication-r2; it does not prove the complete managed network flow or managed cache provenance.

**670 native library tests**, full vendor workspace/all-target Clippy `-D warnings`, and vendor fmt PASS, with exact source/cwd/toolchain/platform/env/UTC/exits/log hashes in native-commands.json. Source clean before/after asserted. Root consumers/10 sidecar/2 CLI passed on unchanged production source53f369ff5 in ../local-publication-r2; those were not repeated for this test-only commit. Actual current/legacy Ed peer compatibility on this source has a separate bundle.

P2 remains incomplete and acceptance reviews have not begun. Required remaining managed publication/closure/cache work is not replaced by this test. D2 is excluded from implementation per the user; no new storage/recovery architecture or scope is introduced.
