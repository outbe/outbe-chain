# P2 intermediate native stream and CLI verification

**P2 remains incomplete and has no acceptance reviews.** Source `6c19aaaf514b7d5bd77c3de3559185459eaf425f`, source-change base `d91a9563df25b8f3c9063f15138e71ef93e15dcd`. No D1 reachable-closure or cache-provenance design was selected/implemented.

The existing COB stream discarded its repository and used raw Git operation loading. After a successful actual runtime BLS fetch, this produced NativeContextRequired for an already verified BLS Review. Stream/iterator now retain the actual repository and invoke existing native signature/publication verification for the current operation. Identity/Issue/Patch stream constructors and the real `rad cob log` caller pass that repository. No Vote/validator/epoch rules were added to Heartwood.

| Behavior | Source and observed proof |
| --- | --- |
| BLS identity and Review streams work after real peer fetch/reopen | `radicle/src/cob/{stream.rs,stream/iter.rs,identity.rs,issue.rs,patch.rs}`, `radicle-node/src/tests/e2e/native_bls.rs`: two actual Ed-transport runtimes fetch sole-BLS identity/refs and ordinary Ed patch, then external BLS Review; identity and patch streams yield correct BLS author/entry IDs. Existing invalid-refs network rejection and accepted-state preservation still pass. |
| Current owner operation cannot skip repository policy | New stream test creates an actual crypto-valid P0 threshold-native COB object under a managed marker installed before repository creation, with a denying provider. all/since/until/range each return the typed publication denial; provider is called exactly four times. A separate ordinary Ed operation remains readable before approval. This does not test or claim complete dependency-closure admission. |
| Legacy stream semantics | All five existing all/from/until/range/repeated-parent regressions pass through actual repository operation loading. |
| Real CLI with Ed and BLS | `radicle-cli/src/commands/cob.rs`, `tests/commands/cob.rs`: existing complete rad_cob_log formula runs actual init/issue/patch/helper operations; a separate actual `rad` process reads an externally signed static-BLS operation in pretty and JSON and verifies exact author, entry ID and Accept verdict. Static test key exists only in the fixture. |

On this exact clean source, full frozen checks all pass:

- **669 native library tests**, zero failed/ignored: radicle405, COB12, crypto21, metadata28, node94, protocol109.
- **2 actual CLI command integration tests**, zero failed/ignored.
- Full vendor workspace/all-target Clippy `-D warnings` and format: PASS.
- Required root sidecar/runtime/keygen/e2e-harness ocomp-integration all-target check: PASS.
- **10 sidecar integration tests**, zero failed/ignored.

`native-commands.json` and `root-commands.json` preserve exact source/argv/cwd/platform/toolchain/environment/start/end/exit/log hashes. The runner asserts fixed source and clean tracked tree before/after. All commands use offline/locked; nested CLI fixture builds use CARGO_NET_OFFLINE=true and installed Rust1.96.0. Authorized local socket/cache access was used. `source-hashes.json` binds all10 affected files across d91→current; artifact index binds the bundle.

Prior 6c9 stream run failed Clippy on the new CLI test's JSON indexing; it is retained honestly in ../native-stream/. The fixed source uses get/pointer without suppression. The earlier raw stream diagnostic is not a clean-SHA acceptance run. Neither failure nor NOT RUN is relabelled PASS.

Still unverified/incomplete for P2: complete managed publication/local/fetch/CAS gate, reachable owner dependencies across types/namespaces, cache provenance/reopen/rebuild and associated receiver negatives. P3 current-material signer, P4 automatic network bootstrap, P6 Review checkpoint, P7 Merge and full P8 E2E remain separate unfinished stages. Network-owned repo has sole BLS authority; no delegation feature or accepted-history revocation was introduced. No full feature or P2 completion is claimed.
