# Scoped managed native-history recovery review

Latest user instruction: implement the necessary functionality; do not add or run E2E tests.
This is a source/targeted-unit review of the remaining functional wiring, not full P2/P8 or feature acceptance.
Base HEAD: `6016e74c7`. Exact working source: `source-manifest.json`, aggregate SHA256 `495e7855588116fe94448d52ce342267ea60e99f8d5348a08452aa4e97eea5d4`.

1. A managed validator pins the existing BLS owner and uses its own finalized receipts, approvals and genesis descriptor. Peer objects do not supply authority, a new Vote decision or an admission grant.
2. Existing native Git transport obtains immutable owner objects before identity admission, retains existing incremental WantsHaves, cancellation and wire limits, and applies no refs/cache in the preparation step. Existing empty repositories can enter this path without requiring a head.
3. Recovery reads actual sigrefs ancestry and native COB/identity signature containers, not arbitrary code blobs. Reads are type/size checked; ancestry traversal is bounded by the local receipt count. Receipt/manifest equality and existing full accepted native proof verification precede each entry grant.
4. Bootstrap, Review, Merge and BindMaterial recover through the existing reconstruction/proof paths; runtime and SDK receive receipt-bound manifests. Accepted reads need no retired shares, DKG Output, attestation slot or witness archive. Missing descriptor is reread from the own chain. Detected Git object absence triggers at most one complete pack retry without haves, followed by the same full proof check; corrupt available bytes remain rejected.
5. A newer own-finalized immutable receipt prefix refreshes a pending recovery plan; conflicting prefixes are rejected. A peer ahead of own finality is rejected until the local finalized view catches up. Existing shuffled seed selection supplies retries; no extra discovery/transport is introduced.
6. Ordinary unconfigured BLS readers, Ed25519 authors and current-generation signing checks retain their existing behavior. No `storage/git.rs`, proposer, consensus I/O, new dependency, or archive-channel change.
7. Scope and evidence are stated accurately. Targeted pure unit checks and root/vendor all-target compilation/Clippy are allowed. No E2E addition/execution or claim of full feature acceptance; broader E2E/CI remains deferred.

The new pure unit fixture checks valid root/BindMaterial reconstruction, exact manifest equality, denied wrong sigrefs and wrong commitment, arbitrary source-blob envelope isolation, no ref promotion during preparation, append/conflict handling, missing descriptor repair through the SDK and manifest-with-empty-ODB recovery state, and a present sigrefs tip with an actually deleted reachable signature blob (MissingObjects and no grants before repair; full verification succeeds after repair). It does not run Radicle nodes or an Outbe chain and is not proof of a network E2E.

Round1 R1 CHANGE found present-tip/missing-blob convergence failure. V2 adds a narrow MissingObjects result and one object-only fetch retry without haves; normal successful preparation retains incremental fetch. Round1 source and reports are archived and are not current PASS claims.
