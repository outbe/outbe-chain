# P3/P6 protected node preparation context

Intermediate source `5dc7c152152c7ab9333aee14be1133826d8172ec`; base `27d8bf28338f3616d284532c780538f93b0db6e1`.

The existing private same-UID node socket now serves two fixed operations:

- OCW1: sequence:u64be + expected receipt hash[32] + canonical manifest. Node reads its own current canonical accepted receipt, checks sequence/hash and manifest commitment, then caches only body/manifest using the existing store. Client checks its configured chain/genesis before sending; a caller cannot provide an authoritative receipt record. Response is status2 or status0, no signing permission.
- OCP1: proposal_id:u256be. Node obtains exact finalized proposal state/public DKG Output through the existing bound actor path, then replays receipt-bound manifests from its own store and own receipt reader. Response contains length-prefixed existing OCM1 context, length-prefixed canonical refs-only manifest and sigrefs SHA1[20]. The refs carrier must contain no objects/updates/subjects; validated root/main refs and nonzero sigrefs become a private-constructed preparation snapshot only after authenticated local exchange.

Maximum request =44+existing1MiB manifest cap. Maximum preparation reply derives from existing public context maximum +existing1MiB manifest cap +fixed framing/sigrefs. Receipt/state/Output clients retain their own narrower response caps. Existing32connections/4blockingworkers/5s deadline and retained worker permits remain; snapshot replay uses another bounded blocking worker after material lookup, with no unbounded worker queue. No namespace,free signing message,Share or native grants are exposed.

For governments-enabled node startup only, the node opens the existing ReviewPackageStore at control-socket path +`.outbe.packages`, transfers it to the protected server, and retains existing observer/shutdown lifecycle. Feature-disabled construction is unchanged. This is production node construction wiring, not a live full-node restart/shutdown claim.

| Contribution | Source | Behavioral evidence in full-libs.log |
| --- | --- | --- |
| Accepted manifest delivery and own-chain receipt binding | local.rs::cache_checkpoint_manifest/serve; local/preparation.rs::cache | preparation_socket_verifies_manifest_receipts_and_replays_after_restart: initially missing manifest refuses preparation; read accepted receipt then cache manifest/idempotent retry; wrong manifest/hash/expected client chain rejected |
| Complete pinned preparation reply | preparation.rs::read/encode/decode; publication.rs::from_local_refs | actual socket + existing actor returns exact state/material hash/refs/sigrefs; server drop/rebind/store reopen restores same result without another upload; missing receipt source despite cache, corrupt cached manifest and withdrawn Output reject |
| Bounded canonical codec | preparation.rs constants and decode; existing material::take/state codec | preparation_context_codec_requires_exact_context_refs_and_sigrefs: roundtrip, truncated header/tail, trailing data,zero sigrefs,oversized nested context and missing root/main refs reject; all earlier context/Output/receipt malformed and overload tests pass |
| Production construction | bin/outbe-chain/src/launch/node/radicle.rs | seven consumers all-target check; adapter+node all-target Clippy -D warnings;6 touched Rust-file fmt PASS |

All71 full adapter lib tests passed/0ignored. Exact argv/cwd/environment/toolchain/platform/times/exits/spec/log hashes in checks.json; changed source hashes in source-scope.json; bundle hashes in sha256.json. Development Oid::as_bytes API mismatch was corrected before this frozen source; only frozen successful runs are claimed.

Limitations: authority/receipts use deterministic fixture readers, with real actor/Unix sockets/filesystem; manifest subjects have real threshold signatures over framing fixtures, not a complete native Git bootstrap package or live canonical chain. Restart is service/store drop/reopen, not process SIGKILL/power loss. The sidecar collector runtime does not consume this SDK API yet; actual narrow Review signer requests, native package/grant/promotion,automatic bootstrap,Merge and production multi-validator E2E remain incomplete. Cached manifests and preparation replies never confer current signing or native entry permission. Actual signing/publishing still requires the existing fresh-state checks and native verification. No task acceptance reviews/closures; D3 genesis description1024->255 proposal still awaits explicit answer, dependent bootstrap/bounds/spec unchanged.
