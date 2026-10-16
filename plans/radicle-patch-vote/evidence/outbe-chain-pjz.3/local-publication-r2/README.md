# P2 current-entry local publication verification

Frozen source `53f369ff532b96e6fe686993f78ca3894e797bf9`; source-change base `0cc376bb8af2f17ea25d7c3b3db76f09c21a3268`. P2 is incomplete; no acceptance review or closure.

Existing trusted current-entry native verifier is now called before local ordinary/draft COB ref writes. Identity Revision additionally uses the existing exact document verifier/receipt. Other identity actions retain their separate semantics. Typed verifier errors reach existing create/update callers, without Vote/Outbe knowledge in Heartwood. See `source-map.md` for all affected writers, callers and fixtures.

The actual externally signed managed identity fixture rejects both ordinary/draft publication without receipts, rejects with only the containing COB receipt, accepts with both exact receipts, and rejects Ed identity substitution while preserving previous accepted document and refs. The actual runtime-node static BLS scenario now publishes both BLS identity and BLS Review through the protected production writer before peer fetch; exact patch, stream, file-backed cache write/reopen/rebuild and forged-sigrefs negatives pass. The policy provider is a receipt-interface fixture, not current Outbe finalized-state verification.

All recorded checks PASS: **670 native library tests**, **2 real CLI command tests**, **10 sidecar tests**, full vendor workspace/all-target Clippy `-D warnings`, vendor fmt, required root consumer all-target check. Exact command/cwd/source/platform/toolchain/environment/UTC/exits/log hashes in native/root command manifests, clean tracked source asserted before/after. The initial 3c9637afa attempt had a test path compilation failure; it remains FAIL in ../local-publication. No failed/omitted check is called PASS here.

Unfinished P2 requirements include complete specified reachable owner-entry admission, complete snapshot promotion/CAS, managed cache receipt provenance/reopen/rebuild and complete managed receiver negatives. This patch verifies current entries only; it does not select/implement a D1/D2 architecture, revoke accepted history, add delegates or complete P3-P8.
