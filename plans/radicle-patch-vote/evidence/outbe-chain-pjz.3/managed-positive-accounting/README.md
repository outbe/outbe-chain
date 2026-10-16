# P2 existing managed positive proof accounting

This source/evidence audit corrects the earlier assumption that the generic managed positive scenario still needed implementation. It already exists on native source c383730c6 and was executed by the frozen671-native-test run in ../current-native-regression. Current file bytes match that source exactly; proof.json records source/log hashes, actual command/times/exit and exact log markers. No new test,source change or rerun was necessary.

| Existing requirement | Source in radicle-node/src/tests/e2e/native_bls.rs | Actual proof |
| --- | --- | --- |
| Generic managed context,sole BLS owner,Ed transport | managed_runtime_nodes_require_receipts_for_bls_owner_but_not_ed_patch -> fetch_native_repository(true) | existing Runtime nodes use externally supplied Storage/provider;context is nonzero and exact entry+kind grants supplied by generic PublicationPolicy; source/receiver transport keys remain Ed25519 |
| Fresh receiver rejects missing trusted provider | guarded runtime branch before ordinary/managed receiver | canonical_repository_absent=true after actual native fetch rad/id verification rejection |
| Missing initial managed receipt does not promote repository | managed branch before receipt grants | managed_bootstrap_without_receipt log and absent canonical repository path; adding exact identity COB/document/sigrefs grants enables actual native fetch |
| Ordinary Ed patch available before owner Review | fetched patch after initial repo fetch,prior to constructing tx.review | exact revision/Ed author asserted,no BLS owner Review; assert_cached_patch exercises write,get,exact revision,list,read-only reopen and rebuild |
| Owner Review waits for generic receipts | managed branch after announcement | managed_review_without_receipt log; old sigrefs and entire previously accepted patch/cache preserved; adding exact Review COB+sigrefs grants enables fetch and native Accept evaluation |
| Accepted output stays accepted after tamper attempt | final invalid-signed-refs branch | altered actual refs payload with retained signature rejected; accepted sigrefs/entire patch/cache/reopen/rebuild retained |

The full existing test is marked PASS in the actual log, not inferred from compilation or helper existence. Native/static variants and all six affected library suites passed together; sidecar10/CLI2/full vendor Clippy/fmt are recorded in the referenced same-SHA bundle.

Limits: receipt provider is a generic deterministic test fixture, not Outbe finalized Vote/current-set RPC. The static BLS fixture deliberately uses a test secret key and does not prove production threshold signing or genesis key provenance. This accounting neither creates new signing authority nor completes P2/P3/feature acceptance. It does not introduce delegated users,revoke accepted history,or reopen the excluded D2/native fetch-gate proposal. Actual managed Outbe integration,signer/control,collector/bootstrap/Merge/production E2E and required acceptance gates remain separately incomplete. D3 description-bound answer is still pending; dependent bootstrap/spec unchanged.
