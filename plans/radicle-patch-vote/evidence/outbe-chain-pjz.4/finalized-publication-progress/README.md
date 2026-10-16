# Own-finalized publication delivery progress — intermediate P3/P6

Source `eef60c359756ed88e723d5294790cf29714f5fac`. `checks.json` records source/spec hashes, exact argv/cwd, environment, Rust/platform, UTC, exits and log hashes. Full77 adapter tests and29 registry tests (all zero ignored), seven consumer all-target builds, adapter/registry/node all-target Clippy with warnings denied and all nine changed Rust files formatting PASS. No source mutation during verification.

## Implemented path and scope

`RadicleRegistry::pending_publication_attesters` reads indices whose existing pending generation and exact body hash match the requested current committee/material/sequence. It uses the same extracted private OPG1 generation calculation as admission, preserving its bytes and every existing write/CAS/admission rule. No storage layout, Vote permission or consensus algorithm changes. The result is delivery progress only; it cannot authorize a native operation, attestation or publication.

`RethSnapshotReader::read_current_publication_progress` samples own current canonical finalized block, opens exactly that state_by_block_hash once, and returns a sealed FinalizedPublicationProgress. An exact accepted receipt is returned before considering current membership/material, preserving accepted history after rotation. A different body for an already accepted sequence rejects. Without a receipt, only the own-current generation's pending attesters are returned; no peer can choose a historic set or state.

The existing protected same-UID local socket adds OCL1 with the477-byte body. Pending replies bind finalized height/hash and exact body hash to a bounded sorted index list (0..255, at most256). Accepted replies reuse the existing canonical ORL1 receipt codec, including chain/body/finality bounds. Unsupported, truncated, trailing, duplicate, out-of-range and mismatched responses reject. Existing blocking-worker/deadline/connection bounds apply.

## Behavioral evidence

- `pending_attesters_advance_to_the_next_batch_without_repeating_the_first_prefix`: actual registry with49 participants and genuine identity signatures. First32 remain pending, repeating them remains pending, observed indices choose the next signature32 and reach q33. Different body observes no progress. Material transition rejects old generation and does not inherit its indices.
- `publication_delivery_progress_uses_one_finalized_state_and_preserves_accepted_history`: actual registry q2 pending/q3 accepted storage states loaded into the Reth exact-hash mock provider at distinct canonical hashes. The current finalized state alone determines pending/receipt; exact accepted receipt survives epoch rotation. Wrong body, chain, absent/noncanonical finality and unavailable exact state reject without latest/pending/archived fallback. Snapshot reads are recorded.
- `publication_progress_socket_binds_exact_body_pending_indices_and_accepted_receipt`: actual Unix socket and trusted reader exercise pending then accepted reply. The maximum256-index frame roundtrips at1103 bytes. Duplicate/out-of-range index, wrong body/chain, truncation and trailing data reject.
- Full existing registry regression includes `publication_maximum_committee_reaches_quorum_with_six_bounded_batches`: n256,q171,six batches, bounded32 identity checks per batch, max storage gas1544500. Existing write-failure rollback, wrong-context and Vote/approval suites remain green.

## Limits and remaining work

Reth's provider and finality are fixture states; the socket reader is a fixture. Registry tests use real signatures and storage behavior, not a running distributed chain/sidecar. This API supplies the authoritative observations an automatic collector needs; the production sidecar scheduler, durable partial journal, retransmission, automatic multi-batch advancement, accepted native installation/announcement, Review/Merge and production E2E are still incomplete. No full task acceptance or closure asserted. No native storage/git.rs changes, push/PR/deploy.
