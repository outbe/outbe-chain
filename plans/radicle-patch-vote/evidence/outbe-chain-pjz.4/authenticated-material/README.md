# Current finalized authority and authenticated partials

Source `26d497f4a2bd5f31582587130bf81cd9a8651eab`. Intermediate P3 evidence only: no task acceptance or independent reviewer PASS.

| Implemented claim | Actual evidence |
|---|---|
| Repository authority comes from the local current finalized hash, never a peer-selected historical snapshot or latest/pending fallback | `manager/snapshot.rs::read_current_repository_authority`; current epoch retained committee matches the same exact-state effective consensus set by address and MinPk key; two Reth exact-state tests reject mismatched/missing committee, missing finality, noncanonical hash and unavailable exact state |
| Canonical DKG players/indices, group key, full polynomial commitment, n and N3f1 threshold agree with finalized state | `integration/signing.rs::RepositorySigningMaterial::new`; real participant-bound Commonware DKG and current-committee fixtures; wrong order and missing polynomial commitment reject |
| OPL1 and ODM1 commitments use exact DKG player order and full canonical public Output encoding | Same material constructor; OPJ1 native contexts retain frozen P0 vector checks |
| A current MinPk identity endorses its actual indexed native partial | Fixed `outbe.radicle.partial.v1`, 268-byte exact frame; message hash, chain/genesis, epoch/version/material hash, index and partial value are covered; no namespace/message parameter accepted by sign_review |
| Authentication and native partial verification precede acceptance/recovery | Review integration test roundtrips all four real frames; rejects wrong sender/address/private identity/local share index, malformed/trailing bytes, tampered fields, valid-key wrong-domain endorsement, other-key endorsement and invalid BLS partial with a valid current identity endorsement |
| Recovery needs q distinct current participants and gives one native signature for different valid subsets | Actual 3-of-4 frames from subsets012 and123 give identical native envelopes; insufficient and duplicate contributions reject |
| Signing, contribution admission and recovery check exact current polynomial, not only the stable group key | `VrfMaterialProvider::{sign_repository,verify_repository_partial,recover_repository}`; real resharing rejects old/mixed partials and an asserted old polynomial under the new version; native history remains valid |
| Exact approved revision is still reviewed/published through normal native objects | Existing full native Review and signed-refs scenario now uses authenticated-frame recovery for Review, actual Ed patch author/later revision, accepted owner snapshot and strict per-entry test receipt provider. Signed refs are threshold-signed by the existing low-level provider; network transport/publication is not claimed |

Frozen checks: all **38 outbe-radicle lib tests**, all **506 outbe-consensus lib tests**, and scoped consensus/radicle lib+tests Clippy with `-D warnings` PASS, no ignored tests. `root-checks.json` contains exact source/cwd/environment/toolchain/platform/commands/timestamps/exit codes/spec hashes and per-log SHA256. The full consensus run includes all 41 Hybrid tests and the real DKG reshare regression, not only filtered crypto checks. Vendor source is unchanged from1341e6de4; native416 tests and vendor lint/fmt evidence remain in `../native-review-foundation`.

## Still incomplete

This adds real library operations and the exact-state reader. The finalized watcher/engine DKG output provisioning is not yet wired to a production signing actor; the DKG output constructor explicitly requires the locally authenticated canonical Output. Test authority/approval/receipt data are explicit fixtures. There is no live distributed collector, durable journal or authenticated node-sidecar exchange claimed by these tests.

The authenticated wrapper currently covers Review COB. Bootstrap, signed-refs and Merge actor paths, bounded publication carrier/system phase/checkpoint/current-set receipt policy, genesis governments bootstrap, actual automatic announcement/fetch and final production multi-validator E2E are still outstanding. No guarantee of receiver rejection based solely on BLS group signature or an archived receipt is claimed. The extra git.rs/fetch gate stays reverted; no D2 work.

Available failed development fixture/compile attempts are in `development-failures/`, separate from frozen-source checks. No external key/OCOMP source was changed to make a fixture pass; tests reuse the existing canonical boundary fixture seam.
