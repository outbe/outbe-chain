# Threshold signer and exact native Review foundation

Source: `1341e6de4c5cf42f7dcbfa575b2c0b8b82a9323d`.
This is intermediate P3/P6 evidence, not task acceptance. No P2/P3/P6 closure or independent reviewer PASS is claimed.

The authorized path is finalized Vote for an exact patch revision -> network threshold BLS Review Accept -> signed refs -> normal native announcement/fetch. The extra native COB fetch gate was reverted in `0f9e5fa495cd9d2418169f13aaf60eeebead9558`; this source does not restore it. `storage/git.rs`, `radicle-fetch/state.rs` and `storage/git/cob.rs` retain their `c11c37e2a` rollback bytes.

## Observed checks

Exact commands, cwd, full source SHA, toolchain, platform, environment, timestamps, exit codes, spec hashes and log SHA256 are in `root-checks.json` and `vendor-checks.json`. All seven commands exited 0.

| Claim | Source and behavioral proof | Artifact |
|---|---|---|
| Current-version fixed-domain native partial signing; indexed partial verification before recovery; insufficient/duplicate/outsider index, wrong group/subject and archived version reject | `consensus/src/hybrid/vrf_material.rs`; 2 real 3-of-4 DKG tests; different 3-participant subsets recover one native Radicle signature; repository/VRF namespace negatives | `repository.log` |
| Actual resharing preserves group key and historical signature while old/mixed polynomial partials reject | `consensus/src/dkg_manager/tests/mod.rs::repository_signing_uses_reshared_polynomial_and_preserves_historical_signature`; uses existing Commonware Dealer/Player round fixture | `repository.log` (3 tests total including above) |
| Prepared Review uses exact approved revision and previous accepted owner tip; later local author revision does not change prepared bytes or obtain network acceptance | `radicle/src/integration/review.rs`; `exact_vote_revision_preparation_ignores_later_author_updates_and_threshold_accepts`; actual native COB evaluation, Ed author, real threshold signature, explicit test receipt provider | `review.log` |
| Exact OPJ1 domain/context encoding agrees with approved P0 vectors | `review_context_matches_frozen_p0_native_vectors`; fixed Review and signed-refs context hashes | `review.log` (2 tests total) |
| Review owner signature is checked; preparation/attachment does not promote refs; resulting native signed refs retain main and link previous signed refs; mismatched owner snapshot rejects | Same Review integration scenario; actual native SignedRefs reader and patch materializer; wrong repo/patch/revision, proposal/network/material replay negatives; patch stays Open, no Merge | `review.log` |
| Existing native materialization still works | Full `radicle` and `radicle-cob` lib suites, 404 + 12 tests, no ignored tests; pinned historical COB evaluation after later update | `native.log` |
| Shared Hybrid/VRF behavior remains intact | 41 Hybrid tests, no ignored tests | `hybrid.log` |
| Scoped compiler/lint/format validation | root consensus/radicle lib+tests Clippy -D warnings; native radicle/radicle-cob all-target Clippy -D warnings; whole vendor workspace fmt check | `root-clippy.log`, `native-clippy.log`, `native-fmt.log` |

## Open claims

- The low-level signer checks the active material version, group key, participant index and polynomial. It does not authenticate a Vote or current MinPk sender by itself. The production actor must reconstruct the job from the exact finalized reader and map current identities to authenticated DKG players before calling it.
- `FinalizedPatchApproval`, signing inputs and accepted receipt provider in the Review integration scenario are explicit fixtures. This is not the complete EVM Vote -> post-finality production collector end-to-end test.
- Authenticated partial frames/transport, effective ValidatorSet/DKG mapping, finalized checkpoint/current-set publication and cancellation/retry on transition remain incomplete. A historic group signature remains mathematically valid; these tests do not prove full receiver freshness/revocation.
- Automatic genesis governments bootstrap, distributed journal/collector/announcement wiring, separate Merge and final production multi-validator E2E remain incomplete.
- No peer fetch, complete root workspace CI or feature-wide acceptance review was run for this slice. Earlier peer evidence is separate and not substituted for exact-source production E2E.
- No full secret reconstruction, public Share export, Ed owner fallback, Vote interpretation in generic Heartwood, extra git.rs fetch gate or D2 work was added.

`development-failures/` preserves available earlier failed/zero-test development attempts. They are not current-source acceptance checks.
