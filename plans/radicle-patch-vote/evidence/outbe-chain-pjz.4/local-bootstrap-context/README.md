# Protected bootstrap public context — intermediate P3/P4

Source e66c446fd; exact SHA/spec/toolchain/env/argv/platform/UTC/exits/log hashes in
checks.json.74 adapter lib tests,7 consumers all-target check,adapter+node Clippy
-Dwarnings and4 touched-file fmt PASS. No source mutation during verification.

integration/local/bootstrap.rs and LocalReceiptClient::read_bootstrap_signing_context
use existing private same-UID UDS. Exact request OCB1,reply OCB1/status2 plus existing
authority binary fields,u16-length OGD1 andu32-length canonical public Output.
Maximum frame derives only from existing n256,descriptor353 and public Output1MiB
protocol bounds. Existing4 blocking read permits,32 connections and5s deadlines
are reused. No signer/share is installed or transported by this context API.

The server samples own current unbound bootstrap state through the Reth reader,
then asks the existing actor for locally authenticated public material. Exact
authority mismatch between samples requires retry. The client verifies chain,
committee ordering/unique addresses,bounded descriptor and full public Output/
polynomial/group binding. The existing OCS1 authority prefix codec is reused;
its wire bytes/remaining patch checks and existing response limits are preserved.

Actual socket+actor test public_output_crosses_the_socket_only_with_the_same_current_patch_context
now also reads bootstrap descriptor256-byte Cyrillic/public Output without a signing
provider. Missing state/Output,publisher withdrawal and head mismatch reject.
New bootstrap_context_codec_binds_public_material_descriptor_and_exact_chain rejects
wrong chain,truncation,trailing bytes,descriptor overbound,corrupt Output,oversized
frame and mixed observed head. Existing patch codecs/max256 committee stay green.

Limits: same-process socket+actor test with fixture DKG/trusted reader,not live initial
chain/DKG,another-UID/Linux/process-kill testing. This exposes preparation context,
not native partial signing over UDS,collector,bootstrap attestation/registration,
ref promotion/restart/join or production E2E. No task closed.
