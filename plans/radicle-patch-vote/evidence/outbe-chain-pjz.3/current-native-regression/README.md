# P2 current native regression

Frozen source `c383730c6b5c090beb5b3ad2cbdc229f4e947030`, vendor tree `41ecc9e52feebe8e85d620c1f05c8f63e8e872f8`.

This is a verification-only run. The prior complete native suite on0dbd83c40 predates12 changed vendor files (COB get_at/preparation/public framing/runtime/test consumers). Later P3 foundation checks covered narrower packages, so old full-suite logs did not prove the current native checkout. source-scope.json lists and hashes the actual12-file delta. No source was edited for this run.

Actual results on this SHA:

-671 tests across radicle/radicle-cob/radicle-crypto/radicle-git-metadata/radicle-node/radicle-protocol library suites;0failed/0ignored. Includes actual native BLS identity+Ed author patch+BLS Review/signed refs between two runtime nodes, invalid refs rejection,static file-backed cache/reopen/rebuild and fresh managed receiver missing-provider rejection.
-Whole vendor workspace all-target Clippy -D warnings and full vendor fmt PASS.
-10 actual sidecar integration tests PASS, including private native UDS/profile/addresses/limits,readable identity/head,bounded disconnect,status connection/request deadlines and partial-read deadline behavior.
-2 native CLI COB-log tests PASS, including BLS actor pretty/JSON output and ordinary author/issue/patch fixtures.

checks.json contains exact argv/cwd,environment/toolchain/platform,start/end/exit,spec hashes and log hashes. This run explicitly uses installed Rust1.96.0 through RUSTUP_TOOLCHAIN override and the existing standalone target cache; it does not prove the unavailable vendor default1.98.0 toolchain. No dependency/version/config changes or toolchain installs were performed.

P2 remains in_progress: these checks prove current native/static behavior, not a complete managed positive flow with a real Outbe trusted receipt service,full receiver policy/promotion/cache provenance integration or full P0-P8 acceptance. No new task reviews or closure. D2 remains excluded and no git.rs/fetch gate was added. D3 description-bound proposal still awaits explicit user answer; dependent bootstrap/bounds/spec unchanged. Next independent P2 proof can exercise a generic managed positive Ed-author/BLS-owner flow through the already existing PublicationPolicy without changing network bootstrap or introducing Vote into Heartwood.
