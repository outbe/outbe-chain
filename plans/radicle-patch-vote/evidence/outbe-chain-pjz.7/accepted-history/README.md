# Accepted history without retired signing witnesses

Source: `bdf0d66cf` (local commit). No Beads task is closed by this bundle.

The history path binds an own finalized receipt to the exact canonical manifest, reconstructs native objects and verifies their BLS signatures. It no longer requires historical public Output, partial transcripts, ready markers or a local attestation slot. Current signing/publication continues to require its own current material and complete q witness.

The governments integration scenario exercises automatic bootstrap, exact patch Vote, Review/Merge, same-key material resharing and restart. Before its final restart, it removes all archived witnesses/ready markers/slots and the bootstrap content artifact, and unpublishes the current public Output. The accepted snapshot restores through sequence 8 and an existing replica completes native fetch.

The fresh empty-store receiver is still a negative scenario. It is not evidence of successful fresh synchronization. A narrow existing-Git acquisition hook remains separate pending explicit authorization after automatic approval review rejected continued security-boundary edits. The unfinished hook was parked in `/private/tmp/radicle-prefetch-pending`; three independent reviews found that it must extract envelopes from native containers, bound candidate processing and preserve incremental fetch before it can be used.

See `manifest.json` for commands, environment, exit codes, hashes and evidence limits. Full production E2E and complete feature acceptance remain open.
