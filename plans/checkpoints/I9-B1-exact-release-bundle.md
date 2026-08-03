# I9 B1 checkpoint — exact native-DCAP release bundle

Status: `PASS`. B1 covers artifact identity, dependency closure and local
reproducibility only. It does not claim accepted Intel hardware evidence, SGX
execution, release benchmarks or production E2E; H1, P1 and E1 remain
fail-not-skip.

## Fixed candidate and signed prerequisite chain

The current B1 candidate is commit
`4e4fc7fd129fdc53e67318efe5b70dc0fcf759a6`, built twice from a clean detached
worktree with `SOURCE_DATE_EPOCH=1785714090`. It supersedes the earlier
`71d6c6832e5784d4b95ee99aaa26105611ba7615` freeze because the H1 release gate
added source-controlled evidence capture, finalization and workflow inputs.
The prerequisite and corrective commits are:

| Commit | Purpose | Signature result |
| --- | --- | --- |
| `6dbd78b073f86e37eecd352c4a51526827f36ffa` | I9 A0 OST3-only activation | Good SSH/ED25519 signature, key `SHA256:L1VVvQWdQea1QNq0LsHasVTmvEoPriaHEKYbC4yHnsA` |
| `416491126345ef0385af851dcb1cecf861412d6f` | exact native-DCAP release graph and project toolchain | Good SSH/ED25519 signature, same key |
| `79037546f34a2df4c0ca73dce87d8d2af6132601` | exact no-feature node identity gate | Good SSH/ED25519 signature, same key |
| `1d50299e43fcc023139daec6cf036d5647deaa42` | compile dev/mock branches out of production enclave and enforce ELF markers | Good SSH/ED25519 signature, same key |
| `62737f31e5106bcd59dd02937a0678b943b45df7` | use the unified pinned project-toolchain image for prepare/sign/view | Good SSH/ED25519 signature, same key |
| `71d6c6832e5784d4b95ee99aaa26105611ba7615` | place the Docker image after all runtime options | Good SSH/ED25519 signature, same key |
| `72496f158041512ae3b9138b9b7350ebca17dc21` | record the superseded pre-H1 B1 freeze | Good SSH/ED25519 signature, same key |
| `4e4fc7fd129fdc53e67318efe5b70dc0fcf759a6` | introduced the superseded two-row Processor/Platform release gate | Good SSH/ED25519 signature, same key |

The final checkpoint document is committed separately so its signature and hash
are not self-referential.

## Frozen toolchain and native-QVL closure

| Input | Exact value |
| --- | --- |
| Target/profile | `x86_64-unknown-linux-gnu`, `release` |
| Rust | `1.96.0` |
| Gramine | `1.9`, source commit `0d1a4b7607592dab4c8a720c962acee3de6b4ca8` |
| Intel QVL runtime/dev | `1.26.100.1-noble1` |
| Intel QPL, host-only evidence capture | `1.26.100.1-noble1`; excluded from the release image and consensus path |
| Intel SGX headers | `2.29.100.1-noble1` |
| Rust base | `rust:1.96.0-bookworm@sha256:64d9b7f60e3abb08d477cad983d0a3743acc53a19369ba4482510184c9c807e5` |
| Gramine base | `gramineproject/gramine:1.9-noble@sha256:bdf2d0ef9bd09fa10684e14fbe822236df35708d58a852209c5f235842ecb6d7` |
| APT source/key inputs | six files with exact hashes in `release/project-toolchain-v1.json` |
| Downloaded Debian closure | 100 files; sorted-ledger SHA-256 `a96ae1783cc9e359b14bc7504ba6c395c55eadec51ddec9f3e73e42f5f292af6` |
| QVL ELF | build ID `663c0acf2b4673c22c66f01112c5f38d856fd5a5`; SHA-256 `4745bc5b46cbdc17a78119ae2db08f54b86ff9077c5ab480f378741396365aef` |
| QVL policy | offline QVL only; QvE, TVL, QPL/PCCS during consensus and live fetch disabled |

Both clean builders download exact package versions, verify the source/key
hashes and complete `.deb` ledger before installing only from that cache with
`--no-download`. `dpkg --audit`, Gramine version and the native-QVL artifact
contract must pass before Cargo starts.

## Exact artifact/feature graph

Every Cargo invocation uses `--locked --release --no-default-features --target
x86_64-unknown-linux-gnu`. Five non-enclave binaries are one feature-empty
cohort. The second cohort contains only package/binary `outbe-tee-enclave` with
exactly `--features native-dcap`. Neither `--all-features` nor `--all-targets`
is used.

| Artifact | Features | Build-A bytes | Build-A SHA-256 | Build-B result |
| --- | --- | ---: | --- | --- |
| `outbe-chain` | none | 194329176 | `becdf49389a77d520cddbe93d9375cb81f292ad68fdbf2b1ecc28ffe076729c9` | byte-identical |
| `outbe-cli` | none | 11697488 | `85e998bb098347cf8b812bb06fc9bd2c2ae04355509cf0b63e5cd4a22e2e7c64` | byte-identical |
| `outbe-feeder` | none | 17160216 | `80ac4c97f9cfc5b8c8c7ce64feab1a7e14f5e84b6cca8c149863e9f104b846b3` | byte-identical |
| `outbe-keygen` | none | 4256856 | `c1936ce290107ca6c67447215ca74fa78daad0add2876c4d89f2c7f9983e058d` | byte-identical |
| `outbe-ocomp` | none | 31648344 | `b055d3531c14673ee16bbff467c51650b585b7e22b1a96a3efc4288a6b485e23` | byte-identical |
| `outbe-tee-enclave` | `native-dcap` | 4775416 | `bbd84795ec26ed6494919144de43374027a5a4e285945e5b3652fe1477c4025a` | byte-identical |

## Independent build evidence

Builder A:

- command: `scripts/release/reproducible-build.sh --no-cache --release-tag
  commit-4e4fc7fd129fdc53e67318efe5b70dc0fcf759a6 --output
  /tmp/outbe-b1-build-a-4e4fc7f`;
- elapsed: `643.19 s`;
- `SHA256SUMS` SHA-256:
  `3207cd09df4546a5412437d992701754aa718893b6c7055908d83eb60db8ca02`;
- canonical manifest SHA-256:
  `2858d4cbb77a2a6b20b515d5d6300aab66593d2e9307d7e761ddfc418d5d321f`;
- resolved package inventory SHA-256:
  `c19802502b267e558f553d85027fd4405e55897b0a54df2ed19bbb7800736b21`;
- all 11 checksum rows passed.

Builder B:

- same command and release tag, output
  `/tmp/outbe-b1-build-b-4e4fc7f`;
- elapsed: `640.73 s`;
- all 11 checksum rows passed;
- every artifact, `SHA256SUMS` and canonical release manifest is
  byte-identical to builder A.

The pinned verifier reported `result: passed`, with all six
`byte_identical: true`, no differences, and evidence SHA-256
`796857ba0c8dd725cc16fe8690d2b2b6f2d599054c255995773991bcdbcaa2d6`.

## Production-enclave audit

Build A has `DT_NEEDED libsgx_dcap_quoteverify.so.1`, `BIND_NOW`, PIE and the
undefined native ABI symbols `sgx_qv_verify_quote` and
`sgx_qv_get_quote_supplemental_data_size`. Manifest generation and the
independent verifier both reject these exact compiled markers:

- mock entrypoint/banner;
- capture-enclave target;
- QVL test trace begin/end hooks;
- explicit development identity seed;
- development offer-secret fallback.

The Build-A scan found none. Generic Rust `OUTPUT_CAPTURE` and PEM parser
`CaptureMatches` symbols are standard-library/dependency implementation names,
not Outbe fixture-capture surfaces. `sgx_ql_qve_collateral_t`, if present in
Intel headers or symbols, names the collateral ABI structure and does not enable
QvE execution; the pinned policy passes null QvE report info and disables
QvE/TVL. Prohibition text mentioning QvE, host verdicts or live fetch is not an
execution path.

The clean red/green sequence found four real wiring defects:

1. the no-feature node version identity differed from the default build;
2. the production enclave ELF still linked a mock banner and dev branch even
   though the `mock` Cargo feature was off;
3. SGX prepare/sign/view used the plain upstream Gramine image instead of the
   project's exact QVL-pinned toolchain image;
4. the Docker image argument preceded `-e`/`-v`, causing Docker to execute an
   environment assignment as the container command.

Each failure was first reproduced by a regression test, then corrected. The
final candidate makes the production ELF marker scan mandatory and uses one
pinned project-toolchain image for build, prepare, sign and view.

## Unsigned Gramine bundle and signing boundary

The two builders independently produced unsigned bundles at
`/tmp/outbe-b1-unsigned-a-4e4fc7f` and
`/tmp/outbe-b1-unsigned-b-4e4fc7f`. Both prepare runs passed the native-QVL
artifact contract. `cargo xtask release sgx compare` reported `result:
identical`, `entry_count: 75`, tree SHA-256
`eeb8be4bcf278dc402e91c85ce3df03235a814a88cdbc7d218b200b0b12ebe8b`;
the comparison evidence SHA-256 is
`e76362c82b315656d7157850e51cd2a6ed568f377bac35b65c6a0fe3b2fba0c9`.
`SHA256SUMS.unsigned` is also identical with SHA-256
`bd389289ed71c92acecee8b1c1591446dd861b5b2798bc568d04d3e416309ee1`.
An independent recursive diff was empty.

B1 pins the release manifest template to `sgx.remote_attestation = "dcap"`,
Gramine `1.9`, `debug=false`, `edmm_enable=false`, `isv_prod_id=1`,
`isv_svn=1`, `max_threads=16`, and installs the exact QVL/runtime closure into
the bundle. The protected testnet signing key is not present in this local B1
run. Therefore B1 verifies the unsigned tree and signer inputs; it does not
claim a production SIGSTRUCT, accepted MRENCLAVE or hardware execution. Those
remain H1/E1 release-workflow evidence.

## Negative and validation ledger

| Gate | Evidence | Result |
| --- | --- | --- |
| Unsupported `aarch64` | build-spec, generator and native-QVL mutation tests | reject |
| Missing/mutated QVL artifacts or headers | exact digest, size, build-ID and dependency-closure tests | reject |
| Wrong feature/package/binary identity | exact six-artifact and two-cohort tests | reject |
| `mock`, capture or trace compiled marker | red generator/verifier tests plus exact release ELF scan | reject |
| `sgx.remote_attestation != dcap` | xtask SGX release tests | reject |
| Missing SGX/quote/collateral at production startup | A0 startup and NodeHost negative matrix | reject before consensus/execution |
| Wrong measurement or signing inputs | xtask SGX bundle verification tests | reject |
| Exact release Python suite | 61 tests | pass |
| SGX release bundle contract | 11 xtask integration tests | pass |
| Enclave identity tests, default and `mock` builds | 5/5 in each configuration; mock binary check passes | pass |
| Native-DCAP release check | exact package/binary/target command | pass |
| `cargo fmt --all -- --check`, `git diff --check` | post-edit | pass |

Full-workspace `cargo clippy -- -D warnings` remains red only on two pre-existing
`outbe-tee` findings outside the B1 diff (`RuntimeEnclaveClient` large enum
variant and a collapsible `if` in `node_host.rs`). Package-only no-dependency
clippy additionally reports two pre-existing type-complexity findings in
`run.rs`/`seal.rs`. B1 does not alter or hide them. `shellcheck` is not installed.

## B1 boundary and next gates

B1 freezes a locally reproducible candidate; it does not activate production
rollout by itself.

H1 must retain fresh accepted Processor-CA evidence for this exact candidate,
with actual Processor and root CRLs. A real Platform node must pass its own
fresh Platform-CA admission through the same public verifier, but no dedicated
Platform row gates every release. P1 must benchmark the exact signed
`gramine-sgx` bundle, including valid, invalid-early, invalid-late and dense
full-block cases on the minimum supported profile. E1 must pass real Validator,
FullNode and 32-validator lifecycle E2E. Missing runner/evidence is failure,
never a skip.

Git push: `false`. PR mutation: `false`. Governance action: `false`.
`bd dolt push`: `false`.
