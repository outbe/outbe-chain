# Intermediate P3/P6: fresh local Review publication attestation

Frozen source: `7ba8dfe03688a17a93cabd0aa59777999f616e43`. Exact argv/cwd/env,
source/spec hashes, toolchain/platform, timestamps, exits and log hashes are in
checks.json. All 50 adapter tests, seven consumer all-target checks, adapter
Clippy -D warnings and scoped rustfmt passed. No full task acceptance is claimed.

| Required fragment | Source | Behavioral evidence |
| --- | --- | --- |
| Attest only a reconstructed and verified Review publication | `integration/network.rs::EndpointNetworkResolver::attest_review`, `NetworkCommand::RepositoryReviewAttestation`; sealed `publication/review.rs::VerifiedReviewPublication` | native scenario supplies the real complete package proof after native q signatures, Git object/manifest and q-witness checks |
| Recheck current finalized Approved permission, Pending stage, exact predecessor and current material per request | `EndpointNetworkState::review_attestation`, `VerifiedReviewPublication::authorize_current` | four independently running local actors, eight fresh state reads each; consumed stage, changed approval timestamp/checkpoint, unavailable reader, wrong output generation and publisher withdrawal reject |
| Exact current participant index/address/MinPk identity | shared `RepositorySigningMaterial::participant_for` used by partial signing and attestation | all four actual identities return their authenticated index; fifth outsider cannot attest |
| Fixed OPB1 bytes and separate publication namespace | `review_attestation` uses only `PUBLICATION_NAMESPACE` and verified `body().to_bytes()` | independent MinPk verification succeeds in publication namespace and fails in partial namespace; four real results roundtrip the actual PublicationBatch carrier codec |

The bounded nonblocking local mailbox and existing deadline carry a sealed proof,
not peer-selected messages, namespaces, heights or bodies. A worker must first run
native Review package verification; no Deserialize constructor creates this proof.
The actor then reads its own fresh current canonical signing state, constructs
material from the engine-published public Output, checks the proof again and maps
its existing local identity key to the effective committee before signing OPB1.

This is the additional current-identity publication guard. Native threshold BLS
COB/refs signatures and their q partial witnesses remain mandatory and unchanged.
Only participant lookup was shared with the existing partial path; threshold
signing/recovery, VRF domains, proposer and consensus execution were not changed.
The new identity-attestation operation does not need or extract a local Share.

The test's complete native package and signatures are real SDK/native Git data.
Each actor runs through actual local channels and signs with its own current
MinPk identity, but uses a fixture current authority reader and NoSend transport.
No live q-identity chain checkpoint, normal peer replication or full-node E2E is
executed. The fresh-state mutations are rejection probes, not real epoch changes.

## Remaining work

Durable job/slot selection belongs to the collector's journal and is not implemented
by this signing primitive. This interface alone does not provide crash-safe
non-equivocation, deterministic approval scheduling or finalized checkpoint delivery.
The collector must persist its selected body before requesting attestations.

Protected node/sidecar UDS, distributed collector/journal, before-publish scheduler,
BindMaterial/rotation/cancellation, automatic genesis bootstrap, separate Merge and
managed normal replication/production E2E remain incomplete. P2/P3 stay in_progress;
no acceptance reviewers/PASS or task closures were requested. No new Git storage/
fetch admission gate, git.rs, D2, Heartwood Vote policy or native wire changes.
