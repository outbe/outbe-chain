# Fixed bootstrap identity and refs signing over local socket — intermediate P3/P4

Source `837397a177990011754d77804d6d812a830b7452`. Exact commands, source/spec hashes, environment, toolchain/platform, UTC, exit codes and log hashes: `checks.json`. 74 adapter tests (zero ignored), seven consumer all-target builds, adapter/node all-target Clippy with warnings denied and all five changed Rust files formatting passed. No source mutation during verification.

The existing protected same-UID socket now supports OCI1 (identity COB) and OCF1 (signed refs). Requests carry the expected base bootstrap body hash, the local bare repository path, the content-manifest blob OID and verified preceding native signature(s). They do not carry arbitrary signing bytes, domains or secrets. The node reconstructs the current deterministic document, verifies the exact genesis content artifact and tree closure, and prepares the native COB/refs subject itself. Existing public Repository::open is reused; no profile or full repository key is loaded and no native storage/fetch source is changed.

| Requirement | Source at reviewed SHA | Behavioral proof |
| --- | --- | --- |
| Fixed identity/refs requests, public authenticated replies | integration/local.rs:295,324; integration/local/bootstrap.rs:194,273 | actual socket test below |
| Fresh own authority/material and typed actor signing | local/bootstrap.rs::current/sign_native; existing EndpointNetworkResolver typed bootstrap methods | missing state and changed descriptor refuse signing; installed existing provider succeeds |
| Response belongs to exact subject/current participant | integration/signing.rs:193,210; local.rs:319,348 | identity partial cannot verify as document partial; three valid contributions recover native envelopes |
| No ref promotion during preparation/signing | existing bootstrap job attach methods | zero raw Git refs after all three signatures and final package attachment |

`integration/local/tests.rs:640`, `public_output_crosses_the_socket_only_with_the_same_current_patch_context`, uses an actual Unix socket and node actor. It creates a bare Git repository directly, without a profile key; obtains local partials for document, identity and refs; recovers each envelope from three authenticated providers in a four-participant fixture; attaches the complete package. Wrong content artifact, truncated/trailing requests, damaged prior envelope, changed descriptor and unavailable state are rejected. Existing context/material/withdrawal tests remain green.

The server reuses existing request/connection/deadline and blocking-worker bounds. Git reconstruction occurs in spawn_blocking; the secret provider remains in the actor, which independently rereads permission before signing. The client verifies each reply with public Output and its typed job.

Limits: same-process socket plus actual actor/Git/crypto with fixture DKG and trusted finalized reader. No live multi-validator ceremony/finality, automatic sidecar scheduling, distributed collection, publication forwarding/promotion, crash recovery or production E2E is claimed. Review/Merge are not implemented by these bootstrap RPCs. No task closed or acceptance PASS asserted. Repowise health query explicitly targeting this worktree returned repository-not-found; no stale main index was used as evidence.
