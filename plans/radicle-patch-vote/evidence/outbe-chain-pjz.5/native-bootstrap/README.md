# Native bootstrap foundation — intermediate P4 evidence

Source: ac8a57811. Exact full SHA/spec/toolchain/platform/env/argv/UTC/exit/log hashes
are in commands.json. All72 outbe-radicle library tests, all-target Clippy -Dwarnings,
format of4 touched Rust files and diff check pass. Actual socket tests ran outside
the sandbox; no sandbox EPERM result is represented as success.

Source map: integration/bootstrap.rs prepares a canonical sole-BLS Doc/RID from
OGD1+authenticated public material, then verifies/attaches the document signature,
prepares identity COB and initial code commit with genesis metadata/tree, then
attaches COB/signs refs and assembles exact object/refs/envelope manifest. It writes
immutable objects only, never refs. Initial content tree closure and exact manifest
artifact hash must match the descriptor. Existing prepare/attach/formatter/closure
APIs are reused; storage/git.rs and fetch paths are unchanged.

verify_accepted_bootstrap reconstructs the entire package against a sealed trusted
receipt and existing AcceptedPublicationVerifier/RepositoryWitnessBundle. Only its
sealed AcceptedBootstrapPublication can grant native root Identity/COB/sigrefs entries
in the existing AcceptedRepositoryPolicy. No arbitrary root or code entry is granted.
Historical proof does not mint current signing permission or revoke prior history.

Behavior: bootstrap/tests.rs runs real native Git objects in two independent storage
directories and genuine threshold MinSig with different3-of4 subsets. Both yield
identical RID/body/manifest with a256-byte Cyrillic description. Checks all committed
object hashes, initial commit/tree/parents/time, absent refs before publication,
wrong manifest/wrong-kind signature/q-1 witness rejection, receipt-bound policy
initial denial/idempotent exact grants/unrelated-entry denial and native identity
Doc/evaluator verification after an explicit test-only accepted COB-ref install.
Other genesis hash preserves content-derived RID but changes signing context;
wrong chain, oversized description and unrepresentable Git timestamp reject.

Limits: the test DKG is an explicit bootstrap_dkg_for_participants fixture, not a
production interactive ceremony. Trusted receipt/authority are test-created. The
factory cannot itself sign, register, finalize, promote refs or announce. Runtime
bootstrap reader/current signer/collector, persistence/recovery/join, actual initial
DKG production E2E and full P4 acceptance remain unfinished. No task is closed here.
