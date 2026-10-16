# Verified bootstrap persistence and current-set attestation — intermediate P3/P4

Frozen sourceb827c090c. checks.json binds full SHA/spec/toolchain/env/argv/cwd/platform/
UTC/exits/artifact hashes.74 adapter tests,7 consumers all-target checks,adapter+node
Clippy-Dwarnings and5 touched-file fmt PASS. Source stayed fixed during verification.

bootstrap.rs now reconstructs native objects identically for current and historical
verification. prepare/verify_bootstrap_publication returns a sealed proof only after
3 actual subjects/envelopes and current authenticated q-witness verification. Current
re-authorization reconstructs the descriptor/material/body binding; accepted history
still uses its exact immutable receipt/public Output.

publication/store.rs reuses the existing complete-package persistence implementation
for bootstrap and Review. Existing private-directory/OS-lock/object hashes/READY/slot
and original-witness reload checks are preserved. New selected bootstrap token is
constructible only by persisting the sealed proof and its exact checked witness.
Loaded bytes still require full native verification before a new token/attestation.
No persisted format/schema or native Git storage/fetch code is changed.

network.rs: attest_bootstrap takes only a selected token; actor rereads own current
bootstrap state/material and validator identity mapping,then signs OPB1 in the existing
publication namespace. No threshold Share is needed for an identity attestation;
this supplements the actual threshold-signed native package.

bootstrap/tests.rs + network/material_tests.rs: actual two storage collectors with
3-of4 native signatures;reconstruct current proof,deny q-1/wrong witness persistence,
persist/drop/reopen/load into a fresh Git ODB/reverify with no refs. Actual actors
produce verified current identity attestations;outsider,missing Output,changed
descriptor,withdrawn permission,material transition and publisher drop reject.
Independent namespace verification rejects use as a repository partial. Existing
Review persistence/alternate witness/corruption/restart regressions remain green.

Limits: fixture initial DKG/current chain state; no actual chain admission/production
bootstrap registry execution yet in this scenario. No sidecar control signing/attestation
request bridge,distributed collector,automatic registration/ref CAS/announcement or
production multi-validator E2E. No task closed or full acceptance claimed.
