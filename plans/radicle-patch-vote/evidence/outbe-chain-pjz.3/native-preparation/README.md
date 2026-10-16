# P2 intermediate native preparation and real runtime replication

**Not P2 acceptance.** Source `d91a9563df25b8f3c9063f15138e71ef93e15dcd`, parent `999b0e643` (P5 evidence only). This checkpoint implements the approved P2 external COB preparation seam and exercises static BLS ownership over actual runtime transport. Managed receiver closure/cache provenance/finality-CAS admission, production current-material signer and network bootstrap are incomplete. No D1 implementation or approval is implied.

## What is proven

| Required behavior | Production source and actual proof |
| --- | --- |
| External preparation without an Ed owner or blocking signature wait | `radicle-cob/src/backend/git/change.rs::prepare_native` writes the existing canonical manifest/tree and returns validated unsigned commit bytes using explicit author/committer. Tips and Related sort/deduplicate; actual Git parents keep the first position and validate object types. No clock, Git config or signer is read and no ref is written. `Transaction::prepare_native` reuses canonical action encoding and embeds/related extraction; empty operations reject. |
| Exact native bytes and external signature | The existing P0 real threshold vector test now calls production prepare; its unsigned bytes match the frozen P0 vector exactly and its existing external threshold envelope attaches to the exact expected native commit OID. Tampered metadata/repo/context/header/key rejects, reopen verifies, refs remain absent. A separate preparation test checks actual ordered parents, embeds, explicit unequal author/committer, missing/noncommit parents and malformed metadata. |
| Sole BLS identity through the real evaluator | `cob/store/native_test.rs` creates a canonical identity document with one BLS delegate and threshold1, externally signs document and containing COB, invokes `Identity::from_root`, then reopens/loads/evaluates the actual entry. No Ed authority or ref promotion occurs. This static BLS test key is test-only; it is not a P3 distributed signing implementation. |
| Two actual runtime nodes, ordinary Ed patch, external BLS Review and refs | `radicle-node/src/tests/e2e/native_bls.rs` starts two real local TCP/UDS runtime nodes with distinct Ed transport keys. Source repository identity's sole owner is BLS. Receiver fetches exact identity and Ed-author patch before any BLS Review, then fetches externally prepared BLS Review Accept and new BLS sigrefs and verifies via production storage/evaluator. Actual repo/owner/identity/patch/revision/review/sigrefs/transport IDs appear in `runtime-vectors.log` and full raw test log. |
| Invalid refs cannot replace accepted state | The live test changes the actual refs payload while keeping the old valid BLS signature blob. The second runtime's production fetch rejects with subject/authority mismatch. Its prior accepted sigrefs and exact patch with Review remain unchanged. This is crypto tampering, not an old-shares or Vote-bypass claim. |
| External publication object access | `VerifiedCommit::oid` exposes the already verified signed-refs object ID to external preparation callers; attachment still writes no refs. The runtime fixture publishes static native refs explicitly; a complete managed policy publication boundary is not claimed. |

## Actual checks on this source

All commands ran offline/locked at the recorded feature worktree with exact source/clean tracked tree assertions before and after. `native-commands.json` and `root-commands.json` preserve argv/cwd/platform/toolchain/environment/start/end/exit/log hashes. Local socket permission and installed Nargo/BB caches were used; there was no remote action.

- Six full native library suites: **668 passed, zero failed/ignored** (radicle404, COB12, crypto21, metadata28, node94, protocol109).
- Full vendor workspace/all-target Clippy with `-D warnings`: PASS.
- Full vendor format: PASS.
- Required root sidecar/runtime/keygen/e2e-harness (ocomp-integration) all-target check: PASS.
- Full sidecar integration suite: **10 passed, zero failed/ignored**.

`source-hashes.json` binds the complete10-file source checkpoint. Those hashes refer to the exact source commit, not later worktree edits. `artifact-hashes.json` binds this intermediate bundle. `run_checks.py` is source-pinned to this checkpoint and intentionally does not bless later changes.

Initial development probes were not acceptance runs: an isolated COB-only command omitted existing serde feature unification and the first fixture had compile/API errors; these were corrected before the frozen complete runs. No failed probe is called PASS. Later stream work is separate: at this checkpoint raw Git stream loading still rejects native BLS with NativeContextRequired; the following P2 change must correct and reverify that path.

No P2 full-AC reviews requested and no P2 task closure. No P3 shares/collector, network genesis bootstrap, Vote-to-Review/Merge, managed cache closure or full feature E2E is established here. Full feature remains active/incomplete; P5 has its separate accepted evidence.
