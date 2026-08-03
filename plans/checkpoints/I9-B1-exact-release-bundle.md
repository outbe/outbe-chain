# I9 B1 checkpoint — exact native-DCAP release bundle

Status: `PASS` as of 2026-08-03 for signed candidate
`267a8fbd361aa003db8a87022ee46309e20c9719`. B1 covers artifact identity,
dependency closure and local reproducibility only; H1, P1 and E1 remain
fail-not-skip.

## Fixed candidate and signed prerequisite chain

The current B1 candidate is commit
`267a8fbd361aa003db8a87022ee46309e20c9719`, built twice from a clean detached
worktree with `SOURCE_DATE_EPOCH=1785744191`. It supersedes the historical
`4e4fc7fd129fdc53e67318efe5b70dc0fcf759a6` freeze and includes the canonical
OCOMP fixture refresh plus the accepted testnet Platform policy. The
prerequisite and corrective commits are:

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
| `ea77c74b50c7d7df2da3e7dffd4672d24b4104b6` | refresh canonical OCOMP release fixtures | Good SSH/ED25519 signature, same key |
| `267a8fbd361aa003db8a87022ee46309e20c9719` | admit `ConfigurationAndSWHardeningNeeded` for the testnet Platform policy | Good SSH/ED25519 signature, same key |

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
| `outbe-chain` | none | 194336040 | `6ea7df741b37264ac82bcf9838c742a451f9ab69e760d3a73d35e6fdf7cbe591` | byte-identical |
| `outbe-cli` | none | 11696856 | `200ae458fb87481d91f299cf712e282a5e84cfb463b2f47ff9212e148d5b2230` | byte-identical |
| `outbe-feeder` | none | 17160216 | `80ac4c97f9cfc5b8c8c7ce64feab1a7e14f5e84b6cca8c149863e9f104b846b3` | byte-identical |
| `outbe-keygen` | none | 4257288 | `40e0339463e691d17e357ff7d3b77e9230c6b769eca178915cb9c07857d53673` | byte-identical |
| `outbe-ocomp` | none | 31990440 | `793f2045606526ad9c0943b377876d9429cac903ac1edb231e70e51f7e120d3d` | byte-identical |
| `outbe-tee-enclave` | `native-dcap` | 4775368 | `7ad25254411d1a00769defcbe138965d588addbda9efeff04e0227335cd2cf90` | byte-identical |

## Independent build evidence

Builder A:

- command: `scripts/release/reproducible-build.sh --no-cache --release-tag
  commit-267a8fbd361aa003db8a87022ee46309e20c9719 --output
  /tmp/outbe-b1-build-a-267a8fb`;
- container build elapsed: `609.8 s`;
- `SHA256SUMS` SHA-256:
  `6d35fb7ad5899447aa1847990bb3def263d971b96a38786c82b9883f0752fc58`;
- canonical manifest SHA-256:
  `de64f6dc5d8e472308ae0b5bb224579cd3dddb174d2da4970306aba8f237300f`;
- resolved package inventory SHA-256:
  `c19802502b267e558f553d85027fd4405e55897b0a54df2ed19bbb7800736b21`;
- all 11 checksum rows passed.

Builder B:

- same command and release tag, output
  `/tmp/outbe-b1-build-b-267a8fb`;
- container build elapsed: `594.6 s`;
- all 11 checksum rows passed;
- every artifact, `SHA256SUMS` and canonical release manifest is
  byte-identical to builder A.

The pinned verifier reported `result: passed`, with all six
`byte_identical: true`, no differences, and evidence SHA-256
`41a30cf8ee7e8034cded3102913e251aaf848ef8a56aa1dda658b676c897717e`.

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
`/tmp/outbe-b1-unsigned-a-267a8fb` and
`/tmp/outbe-b1-unsigned-b-267a8fb`. Both prepare runs passed the native-QVL
artifact contract. `cargo xtask release sgx compare` reported `result:
identical`, `entry_count: 75`, tree SHA-256
`dbbe1863a43cf613c0f43ce7ff0d8075ea0435f59798bdffeaf5dce7db20cd73`;
the comparison evidence SHA-256 is
`df23009329bed8745bcaf9c640964363068b087f99349d7bde19a79a1b0b25f5`.
`SHA256SUMS.unsigned` is also identical with SHA-256
`9e5304a56270f23b7b095d8350597c1cf8f829779d4dcffe3a1fdda74a182706`.
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
| SGX release bundle contract | 12 xtask integration tests | pass |
| Native-DCAP release check | exact package/binary/target command | pass |
| `cargo fmt --all -- --check`, `git diff --check` | post-edit | pass |

No production code changed during this refreeze. Full-workspace cleanup and
unrelated lint findings are outside B1 and were not expanded into this gate.

## B1 boundary and next gates

B1 freezes a locally reproducible candidate; it does not activate testnet by
itself. Candidate `267a8fbd361aa003db8a87022ee46309e20c9719` is the exact
input for H1, P1 and E1. Any later source change invalidates this checkpoint
and requires a new B1 freeze.

H1 must retain fresh accepted Processor-CA evidence for this exact candidate,
with actual Processor and root CRLs. A real Platform node must pass its own
fresh Platform-CA admission through the same public verifier, but no dedicated
Platform row gates every release. P1 must benchmark valid, invalid-early and
invalid-late QVL paths plus the maximum reachable full-block workload for the
exact signed `gramine-sgx` bundle on the same SGX server. E1 must pass the
reachable Validator and FullNode `DcapRequired` paths. A logical or physical
32-validator network is not a release gate. Missing required runner/evidence
is failure, never a skip.

Git push: `false`. PR mutation: `false`. Governance action: `false`.
`bd dolt push`: `false`.
