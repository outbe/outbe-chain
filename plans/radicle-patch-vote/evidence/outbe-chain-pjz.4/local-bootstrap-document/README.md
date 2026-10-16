# Fixed bootstrap document signing over protected local socket — intermediate P3/P4

Source `cc50a5b402ef4046154bebd8a2d4fa9186d250a6`. `checks.json` records exact source/spec hashes, argv, cwd, environment, toolchain/platform, UTC, exit codes and log hashes. 74 adapter tests, seven consumer all-target builds, adapter/node all-target Clippy and touched-file formatting passed.

`LocalReceiptClient::sign_bootstrap_document` uses the existing same-UID protected socket. Request OCJ1 contains only the expected bootstrap body hash. The node reads its own current unbound descriptor and authenticated public Output, reconstructs the exact native identity document and delegates to the existing actor. The actor rereads current permission and uses its installed provider. Response contains only the authenticated partial; no Share leaves the provider.

The actual socket/actor test `public_output_crosses_the_socket_only_with_the_same_current_patch_context` exercises missing signer, installed provider success, incorrect chain/job hash/descriptor, unavailable state and material withdrawal. Client checks participant authentication and exact native subject with public material.

This is fixed document signing only. It does not implement identity/refs socket signing, peer collection, automatic bootstrap/promotion, Vote execution, Merge, restart or production E2E. No task acceptance or closure is asserted. DKG/state and node actor are test fixtures, not a deployed ceremony/finality proof.
