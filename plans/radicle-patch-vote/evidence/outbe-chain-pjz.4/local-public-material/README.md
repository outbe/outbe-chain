# P3 protected public material evidence

Intermediate source `f487b894c319b0f93d20abc0e308431166676a7f`; base `978acc69eb347dc0dea960b6e561f841080b31f4`.

Adds `OCM1 || proposal_id:u256be` (36 bytes) to the existing authenticated same-UID/private Unix socket. The response is status0 (unavailable) or status2, a length-prefixed existing OCS1 exact finalized patch state, then length-prefixed canonical Commonware public Output. Limits derive from existing state maximum and existing1MiB public Output cap; server request cap36, receipt response cap562 and state response cap are enforced before allocation. No secret Share/provider is serialized or transferred.

The server reads current patch state via its bounded blocking-reader pool, asks the existing bounded actor resolver for current public material, and requires exact authority (including observed finalized block), material hash and players commitment equality. A transition between those reads returns unavailable. The client verifies its expected chain/proposal, all existing state bindings, canonical Output, exact participants/order/q/group/polynomial, and material hash again before returning typed state/material. These remain preparation inputs: future signer/publisher calls must recheck current authority as already required; this read is not durable permission to sign.

| Contribution | Source | Behavioral evidence in full-libs.log |
| --- | --- | --- |
| P3 AC1: current authenticated public Output provisioning | integration/local/material.rs; local.rs read_patch_signing_context; node/radicle.rs with_material_resolver | public_output_crosses_the_socket_only_with_the_same_current_patch_context: actual Unix socket + existing EndpointNetwork actor, matching public Output exact bytes/material hash; signing provider/Share deliberately not installed |
| P3 AC1/AC2: mismatch/freshness fail closed | material.rs bind/read; existing network.rs current_repository_material and material.rs::current | same socket test: initially missing Output, wrong proposal, finalized-head mismatch between reads, matching next snapshot, same group key with wrong generation/checkpoint, missing current state, Output withdrawal, publisher Drop, actor shutdown all reject or converge as expected |
| Public wire trust boundary | local/material.rs decode/take and existing signing.rs::from_canonical_output | public_output_codec_binds_exact_material_and_rejects_corruption_and_lengths: wrong proposal/genesis, truncations, suffix, state/output oversized lengths, corrupt Output, internally consistent state with wrong material hash reject |
| Existing guarantees | prior local state/receipt source and tests | all prior real socket owner/permissions/lifecycle, overload/deadline, unchanged ORL1, exact-state codec and n256/257 tests remain passing |
| Production wiring / consumers | node/radicle.rs + inferred existing resolver type | all-consumers.log7 packages all-target; adapter+node all-target Clippy -D warnings and6 touched Rust-file fmt PASS |

`checks.json` contains exact source/spec hashes, argv/cwd/environment/toolchain/platform/times/exits/log hashes. All66 full adapter tests passed/0ignored. `source-scope.json` records changed-file hashes; `sha256.json` covers this bundle.

Not full P3 acceptance: fixture authority readers are deterministic local test readers, not a live node; DKG fixture is centralized test material, not interactive ceremony or deployed key provenance. No sidecar collector consumes this SDK API yet; narrow signer RPC, automatic bootstrap, ref promotion, Merge and full multi-validator production E2E remain incomplete. No new task reviews/closures. D3 description1024→255 decision remains unapproved; dependent bootstrap/bounds/specs unchanged. No cross-UID/Linux or live full-node shutdown claim.
