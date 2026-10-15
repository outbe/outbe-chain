# P2 intermediate evidence: keys, metadata and wire

Source commit: `f1c224eda9907565c9810c8987c64e4efd2a804a`. This is not P2 acceptance.
`commands.json` records actual argv/cwd/environment/platform/toolchain/times/exits
and log digests. All four recorded checks passed: root required all-target check,
whole vendored workspace/all-target Clippy with warnings denied, 648 native tests
(401 radicle +18 crypto +28 metadata +93 node +108 protocol), and fixed production
wire vectors. The regression suite includes real local TCP/UDS node tests.

Two separate subprocess peer scenarios passed, using built binaries whose exact
revision and SHA256 digests are in `peer-current/result.json` and
`peer-legacy/result.json`. They cover Ed25519 init/clone/fetch, exact commit/content,
identity/delegate verification, signed refs and remote-helper inspection. The
legacy author binaries are b76a178, the receiver/tooling are the pinned source
above. Their commands are in peer-transcript.json and sidecar logs are preserved.
These scenarios establish Ed transport compatibility, not BLS repository fetch.

The optional whole-suite --all-features attempt did not execute: offline cache
lacks dhat0.3.3 (profiling). Its actual exit101 is preserved in
all-features-unavailable.log. It is not a PASS. No profiling build, remote CI,
Linux/SGX run or full-chain E2E is claimed here.

Remaining P2 work includes native BLS identity/COB/sigrefs prepare/attach/verification,
trusted generic publication-policy coverage for managed owner entries in all
local/fetch/evaluator/cache/direct/reopen/rebuild paths, BLS namespace CLI boundary
checks and real two-node BLS acceptance plus negatives. Lower native COB storage
still uses legacy Ed25519 signatures. BLS RepositoryKey-to-Ed verifier conversion
fails closed. This bundle cannot close P2 or unblock its three acceptance reviews.
