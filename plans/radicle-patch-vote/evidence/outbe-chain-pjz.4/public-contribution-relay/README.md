# Public contribution relay — intermediate P3/P6

Source `645ebf4d47516261505e5fad57f97c4054979f7b`. Exact source/spec hashes, paths, argv/cwd/env/toolchain/platform, UTC, exits and log hashes: `checks.json`. 75 adapter tests (zero ignored), seven consumers all-target build, adapter/node all-target Clippy with warnings denied and eight touched Rust files formatting PASS. No source mutation during verification.

## Implemented production path

`integration/network/gossip.rs` defines OSIG V1 frames for one authenticated repository partial (ordinal0..2) or one publication attestation. Maximum frame584 bytes stays below the existing endpoint-channel frame size. Outgoing messages pass own-current authority/material and own actor-identity verification; the existing checked sender sends to current committee peers except self. No new channel, socket listener, quota or native Heartwood transport is introduced; existing quota/send feedback determines accepted recipient count.

Incoming OSIG messages enter the existing EndpointNetwork receive path. Current public Output authenticates the frame network/generation, participant index and MinPk signature against its transport peer. Attestation messages additionally verify the exact body signature. Outsiders, wrong peer/index, altered signature, wrong context and malformed encodings do not enter the inbox. This is relay authentication: the collector still verifies the specific native subject and polynomial partial before journaling, and a complete native/q package before attesting. The existing native full verifier retains both checks after extracting its shared identity/context predicate.

Inbox keys are (current peer, ordinal0..2 or attestation slot3): at most4*n entries, n<=256. Duplicates replace one slot. A different authenticated material hash clears old inbox entries before insertion/drain; unavailable material clears the inbox on drain. The protected same-UID socket provides fixed OCG1 send and OCT1 take commands. Reply length/count is bounded by4*n; sender keys and messages are decoded and reverified against the client's public material. Messages contain no share/private key and grant no repository permission.

## Actual behavioral evidence

- `public_output_crosses_the_socket_only_with_the_same_current_patch_context`: actual UDS, actor and native bootstrap signing. Own partial/attestation go through actor sender to exactly the other three current keys, with exact encoded bytes recorded by a CheckedSender fixture. Outgoing peer impersonation is denied. Receiver gets wrong peer, outsider and corrupted frames followed by a valid peer frame; only the valid frame is returned through UDS, with exact sender/payload. A second take is empty. Partial truncation/trailing encodings reject. Existing package/attestation/mailbox, native recovery and stale-context tests remain green.
- `relay_inbox_is_bounded_by_current_participant_and_step_and_discards_old_material`: four authenticated fixture providers produce signed contributions. Ten repeats of every partial plus one attestation per participant occupy exactly16 slots. Drain returns16 then zero. Wrong sender/outsider/tamper, invalid version/tag/ordinal/length/trailing frames reject. Epoch/material update clears old data and rejects old frames; withdrawal leaves an empty unavailable inbox. Both canonical frame variants roundtrip.

## Limits and remaining work

The CheckedSender is a recording fixture; injected incoming messages use the existing in-process authenticated transport fixture. This is not a live multi-node peer-network or production collector E2E. DKG/finalized state are fixtures; it does not prove deployed ceremony provenance or future-node rotation liveness.

Sidecar automatic collector scheduling, durable pre-checkpoint partial journal, periodic retransmission/recovery, progression through multiple32-entry batches using finalized tallies, accepted native installation/announcement and full Vote-to-Review/Merge remain required. Relaying authenticated data is not native approval/publication. No stage, review or feature closed. No git.rs/storage/fetch rewrite, push/PR/deploy.
