# Intermediate P2/P3: exact accepted Review verification

Frozen source: `3ffebe3d4814165afacee024c8537acaf123a534`; component base:
`2ddfe4a5382511f83d4ba4ae2f31a7a0b4ce591a`.
All 53 adapter lib tests, seven consumer all-target checks, adapter Clippy with
`-D warnings`, and formatting of the six changed Rust files passed. checks.json
records source/spec hashes, actual argv/cwd/environment/toolchain/platform,
start/end times, exit codes and log hashes. Source was clean after checks.

| Required fragment | Source | Behavioral evidence in full-libs.log |
| --- | --- | --- |
| Exact accepted history remains verifiable without current signing authority | signing/accepted.rs::AcceptedPublicationVerifier, signing/witness.rs::verify_accepted | exact_vote_revision_preparation_ignores_later_author_updates_and_threshold_accepts verifies two real q-subsets after local providers switch from version 7 to 8; old current signing/recovery rejects |
| Receipt binds the exact public Output, manifest and envelopes | from_receipt, verify_witness | changed Output, receipt hash/future height, wrong manifest, reordered/missing subjects and a different crypto-valid owner envelope reject |
| A valid group signature alone is insufficient evidence of q identity contributions | shared verify_native_witness and verify_native_partial | q-1 frames reject; existing wrong sender/index/domain/material/duplicate frame checks still pass |
| Receiver checks actual native bytes, object inventory and refs, not a claimed subject | publication/review.rs::verify_accepted_review_publication, shared reconstruct_review | separate receiver Git storage reconstructs exact accepted package; wrong predecessor and changed immutable approval timestamp reject |
| Accepted proof cannot authorize a new signing job | private AcceptedPublicationVerifier/AcceptedReviewPublication fields; no signing methods or conversion into current/selected proof | source API evidence; existing current signing/attestation gates remain exercised by the full suite |

The accepted verifier takes a sealed FinalizedPublicationReceipt from the trusted
local chain reader. The old OPL1 commitment is trusted from that immutable receipt;
ODM1 binds its epoch/version/OPL1 and complete canonical public DKG Output. No old
committee is synthesized from peer claims, and no current material is substituted
for the accepted material. Exact committed envelopes and reconstructed native
subjects are both checked. This path never reads or exports a secret Share.

Current preparation and historical verification share native reconstruction and
partial verification, preserving their fixed domains and object construction.
Reconstruction writes immutable objects only; it does not install references.

This is intermediate evidence. Receipt/approval/finality are sealed fixtures in
the native scenario, not a live executed multi-validator checkpoint. Provider
version activation is not actual resharing or a membership transition. No runtime
accepted policy, protected UDS, full collector journal/FSM/distributed collection,
automatic bootstrap, Merge or production E2E is claimed. No task is closed and no
acceptance review PASS is claimed. No vendor git.rs/fetch-gate/D2 changes.
