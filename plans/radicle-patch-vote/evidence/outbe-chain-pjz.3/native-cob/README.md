# Intermediate native COB backend evidence

P2 remains in progress. The source commit is recorded in `checks.json`; the checks ran immediately before that commit on identical source bytes. Exact command start/end times were not captured, so this is not the final frozen P2 acceptance evidence.

Implemented: shared Ed25519/BLS repository signature codecs and native subject verification; strict legacy-string/BLS-object JSON; opaque verified BLS COB proof bound to actual commit/tree/actor; exact prepared Git metadata attachment without refs changes; raw Git loaders require explicit repository/context for BLS and reject mixed headers. Existing author/history types and callers use RepositoryKey.

`native-tests.log`: 663 tests PASS, including actual Git attachment, Commonware threshold fixture commit OID, load/reopen, context/repo/metadata/trailer tampering, immutable proof binding, legacy Ed compatibility and mixed header rejection. Full vendor workspace/all-target Clippy and required root consumers pass. The standalone COB crate also builds without git2.

`native-tests-sandbox-denied.log` is an actual failed earlier run: local socket operations returned EPERM. The complete six-package suite was then repeated with permitted local TCP/UDS and passed. It is not counted twice. Earlier compile/Clippy iterations were corrected before the recorded source commit; no success is claimed for them.

Remaining: identity actions, signed refs, generic PublicationPolicy across every managed-owner admission/promotion path, production storage/stream/cache wiring and real two-peer BLS acceptance. The public lower-level native API accepts an independently supplied repository/context; it does not establish current-validator authorization by itself. No P2 review child is closed.
