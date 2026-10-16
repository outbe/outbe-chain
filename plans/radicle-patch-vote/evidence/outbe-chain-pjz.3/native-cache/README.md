# P2 native BLS patch cache after actual peer fetch

Source `0cc376bb8af2f17ea25d7c3b3db76f09c21a3268`, base `fe523719b`. Only production-path test extended; no production storage/policy changes.

`radicle-node/src/tests/e2e/native_bls.rs::two_runtime_nodes_fetch_bls_identity_ed_patch_and_external_bls_review` now uses the real file-backed SQLite patch cache at the receiver. For each state it writes the patch through `Cache::write`, checks get/exact-revision/list equality, closes the DB, opens `StoreReader` and checks get/exact-revision/count. It then rebuilds through `Cache::write_all` (asserting every callback result succeeds) and repeats exact read checks after closing/reopening. The same DB is reused across states to test updating the existing cached representation.

States: ordinary Ed patch before any BLS Review; exact externally signed BLS Review after actual peer fetch; preserved accepted patch after the receiver rejects a signature-invalid refs payload. Equality covers the entire patch, including BLS actor keys in reviews and Ed author, not just counts. Identity and signed refs still use the existing actual production verifiers. Source test retains exact object IDs/transport IDs and invalid-fetch result in logs.

All **670 native library tests**, full vendor workspace/all-target Clippy `-D warnings`, and full vendor fmt PASS on frozen clean source. `native-commands.json` records exact argv/cwd/platform/toolchain/environment/UTC/exit/log hashes; runner asserts source/clean tree before and after. Root consumer/CLI code did not change in this test-only patch, so those checks were not repeated here; prior exact-source b7ee1e165 results remain in native-identity/identity-ed-compatibility.

This covers static native BLS serialization and real patch cache write/reopen/rebuild. It does not prove managed receipt provenance/cache policy or complete P2 acceptance. P2 remains in_progress, with acceptance reviews blocked pending complete criteria. No Vote, collector, Merge or full-feature claim.
