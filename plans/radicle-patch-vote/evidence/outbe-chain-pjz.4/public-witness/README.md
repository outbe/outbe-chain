# Intermediate P3/P6: independent public-material q-witness verification

Source: `83f6690b9d399028548fbe61af2b47861df16572`. `checks.json` records exact
commands/cwd/env/toolchain/platform, source/spec hashes, timestamps, exits and log
hashes. All 49 adapter tests, seven consumer all-target checks, adapter Clippy
-D warnings and scoped rustfmt passed. This is partial implementation evidence.

| Required fragment | Production source | Behavioral evidence |
| --- | --- | --- |
| Bounded canonical public DKG Output decoding against local finalized authority | `integration/signing.rs::RepositorySigningMaterial::from_canonical_output` | native Review scenario decodes actual SDK Output, matches material hash; empty/truncated/trailing and greater-than-one-MiB public-output input fail |
| Receiver independently verifies current indexed q frames for each reconstructed native subject, without access to Share | `RepositorySigningMaterial::verify_witness`, existing `recover_subject` and authenticated partial verification | `exact_vote_revision_preparation_ignores_later_author_updates_and_threshold_accepts` invokes `assert_independent_witnesses` for native COB and signed refs; all four frames and sorted subsets 012/123 recover the exact native envelope |
| A valid owner signature cannot replace required partial witnesses | same verifier | valid native envelope with zero or q-1 frames, duplicate/unsorted/out-of-range frames, more-than-n frames, invalid last frame after three valid frames, forged identity, re-endorsed wrong participant partial, wrong namespace and mismatched native subject/envelope fail |

The public Output decoder caps input before SDK decoding, caps committee decoding
at 256 and requires exact consumption and byte-identical re-encoding. Existing
material validation checks effective participant order, group key, full polynomial
hash and N3f1 threshold against sealed local finalized authority. As documented
in the API, its computed ODM1 hash must also be bound to the applicable checkpoint
by the package publication layer; this constructor alone grants no permission.

Witness verification derives each MinPk identity from the authenticated participant
index, enforces strictly sorted distinct frames and q..n count, then reuses the
production frame context/identity/partial checks. Its local verifier provider has
`share=None`. It recovers and verifies the native group signature and compares the
complete resulting envelope with the claimed envelope. Every supplied frame is
checked, including a bad final frame after a valid quorum. There is no arbitrary
signing request, key reconstruction or secret-share access in this path.

The native test has actual SDK partials obtained through the local actor bridge;
it reconstructs exact native Review/refs subjects. Its bootstrap/checkpoint and
current authority are fixture providers. This is not a live multi-node checkpoint
or rotation scenario. No changes to engine/consensus code were needed in this
commit; their preceding frozen regression evidence remains .4/current-signing.

## Outstanding claims

This verifier checks cryptographic witnesses for a subject reconstructed by its
caller. It is not the complete independent actual-Git-object/package verifier,
serialized whole witness-bundle codec/one-MiB total-bundle enforcement, protected
UDS, freshness scheduler before attestation/publication, collector/journal,
automatic bootstrap, Merge or production E2E. Those remain required by the plan.
No P2/P3 task closure or acceptance reviewer/PASS is claimed. No storage/git.rs,
fetch-gate, D2, Vote interpretation in Heartwood, or native wire format changed.
