# Verified bootstrap publication bridge — intermediate P3/P4/P6

Source `409bbe3813455278f34bb4dc3e3ee32689e412d3`. Exact source/spec hashes, changed files, argv, cwd, toolchain/platform/env, UTC, exits and log hashes: `checks.json`. Full 74 adapter tests (zero ignored), all-target builds of seven consumers, adapter/node all-target Clippy with warnings denied and all ten changed Rust files formatting passed. No source mutation during verification.

## Production path

`integration/local/publication.rs` adds fixed OCA1/OCQ1 requests on the existing same-UID private socket. A request contains an absolute local bare-repository path (u16 length), content-artifact blob OID, exact OPB1 body, length-bounded canonical native manifest and separate q-partial witness. OCQ1 additionally contains one existing bounded PublicationBatch. Frame limits derive from existing manifest/witness/committee/batch bounds, not an unbounded serialized package.

The node reads its own fresh bootstrap state/authenticated public material, reconstructs all three native operations from actual Git objects, verifies exact genesis artifact/tree, signatures, body/manifest and q current participant proofs. It persists the complete verified package and selected body in ReviewPackageStore before the existing actor can attest. No caller bytes/domain/private key reach the secret provider. Git reconstruction and package persistence run in bounded blocking workers outside consensus execution.

The actor independently rereads current permission/material after persistence. Attestation returns the existing canonical body/one-entry batch; client checks the exact body and participant identity signature. Queueing checks every sorted current identity signature and exact selected body before replacing the one-batch mailbox. Node launch wires this same mailbox from ConsensusExecutionBridge to EndpointNetworkService; the existing proposer only takes the budget-fitting prefix. No new Git storage/fetch admission code is involved.

The actor retains only the selected bootstrap token associated with the queued batch. Finalized snapshot refresh and authenticated material updates recheck permission; unavailable/consumed bootstrap permission or changed material clears queued output. Material-publisher closure and actor shutdown also clear it. Retained package files remain immutable; this does not revoke accepted history. Collector retry/recovery and complete candidate-boundary integration remain required below.

## Behavior actually checked

The existing actual socket/actor/Git test `public_output_crosses_the_socket_only_with_the_same_current_patch_context` now executes document/identity/refs partial recovery, complete package verification, durable local selection, actor attestation and block-mailbox forwarding.

- Missing q proof and wrong content artifact: no selection/attestation.
- Truncated/trailing publication requests: rejected.
- Bad final batch signature: no slot selection and empty mailbox.
- Valid package: exact body/manifest load from store; repeated attestation identical.
- Reply/current-participant verification rejects out-of-range index, wrong material version and other signature namespace.
- Three-entry valid batch queued; budget for two entries returns two, without consuming the ready batch.
- No native refs are promoted during preparation/signing/queueing.
- Own state unavailable: new attestation/queue rejected; finalized refresh clears old ready batch.
- Generation update, public material withdrawal and publisher drop: old ready batch is cleared; current restored context can requeue.

## Limits and remaining work

This is a real local UDS/actor/Git/crypto integration test with fixture DKG/finality/state. The local validator attestation comes from the running actor; two additional batch signatures use fixture validator identity keys. It is not distributed peer collection, a deployed DKG ceremony, live canonical finality, actual multi-validator proposal execution or a process-crash E2E. The real registry admission fixture is separately recorded in `.5/bootstrap-registry`.

P4 automatic sidecar bootstrap and accepted native installation/announcement, P6 distributed durable collection, Review control, BindMaterial rotation wiring, P7 Merge and P8 production E2E remain incomplete. No task acceptance, review PASS or closure asserted. Native storage/git.rs is unchanged. No push/PR/deploy.
