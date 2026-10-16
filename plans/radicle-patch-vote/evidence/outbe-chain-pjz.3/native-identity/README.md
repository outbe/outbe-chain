# P2 intermediate native identity verification

Source `b7ee1e16536e80ab4295ca3017303179b8371d47`, base `cb70807a2898078409c99411b4db1843becddc10`. P2 remains incomplete; this bundle is not an acceptance review.

`Doc::load_at` previously returned a structurally valid embedded BLS document without verifying its containing native identity operation. The diagnostic test changed actual signed commit metadata but retained the original signature and observed an accepted document (`diagnostic-before.log`, development diagnostic, not a clean-SHA acceptance run).

The direct read now reuses native entry verification, binds the embedded document to the actual Identity Revision action, verifies the inner document signature, and requests generic publication authorization for both exact containing-entry subjects. The managed marker pins the sole BLS identity authority. Ordinary Ed authors retain native COB loading; an Ed identity cannot substitute for the managed owner.

| Requirement covered | Source / behavioral evidence |
| --- | --- |
| Direct BLS identity signature and exact body binding | `identity/doc.rs::Doc::load_at`, `storage/git/cob.rs::load_entry`; actual external BLS identity test rejects changed commit metadata, wrong inner signature, and mismatched document blob; original document reopens unchanged. |
| Managed exact current-entry authorization | `storage/publication.rs::Registry::authorize`, `cob/store/native_test.rs::direct_managed_identity_read_requires_both_current_entry_receipts`; no receipt rejects, COB-only receipt permits low-level COB loading but rejects document access, both receipts permit document access, valid Ed identity cannot replace managed BLS owner. The receipt provider is a fixture, not an implemented Outbe finality verifier. |
| Actual native peer / Ed / CLI regressions | Existing two-runtime BLS identity/Ed patch/BLS Review fetch and invalid signed-refs rejection; existing Ed suites and two real CLI command tests run on the same source. |

Verified all command manifests and their log hashes: **670 native library tests** (406 radicle,12 COB,21 crypto,28 metadata,94 node,109 protocol), **2 CLI integration tests**, **10 sidecar tests**, full vendor workspace/all-target Clippy `-D warnings`, vendor format, required root consumer all-target check: all PASS. Exact argv/cwd/toolchain/platform/environment/UTC timestamps/exit codes and hashes are in `native-commands.json` and `root-commands.json`. Source hashes bind all four changed files. The runner asserts the exact source and clean tracked tree before/after. This records completed runs; it does not claim repeated execution.

Remaining P2 work includes the complete specified managed admission, reachable owner-entry and cache provenance paths, with actual receiver negatives and acceptance reviews. No distributed signer, automatic genesis bootstrap, post-finality collector or Merge completion is claimed. No D1 or D2 proposal is approved or implemented by this patch. Accepted history remains accepted.
