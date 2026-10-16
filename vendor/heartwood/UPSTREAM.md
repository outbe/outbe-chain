# Upstream provenance

- Project: Radicle Heartwood, https://github.com/radicle-dev/heartwood.git
- Stable tag: `releases/1.10.3`.
- Peeled source commit: `7e1cb406bb935c3de008d2171e8961f57b4336ea`.
- Previous Outbe fork: `outbe/outbe-heartwood` at
  `b76a17801329291153585ed31db61ee3c658046e`, based on upstream
  `16de8f1950825672fe48b901dafa0d009f612b04`.
- Update: three-way transfer from that upstream ancestor. All 67 upstream changed
  paths were applied, including modes and symlinks. The intersecting CLI manifest
  and node config preserve the Outbe delta; the removed `public_seeds()` test uses
  the new empty preferred-seeds default. The transfer manifest is in
  `plans/radicle-patch-vote/evidence/outbe-chain-pjz.2/upstream-transfer.json`.
- Import: complete tracked source tree including licenses and standalone
  workspace/lockfile, without a nested Git repository. The Outbe runtime stays in
  `crates/system/radicle` (`outbe-radicle-runtime`); its only binary entry point is
  `bin/outbe-radicle` in the enclosing chain workspace.

## Retained Outbe delta against stable

- `crates/radicle/src/node/config.rs`: isolated `Network::Outbe`, no public
  bootstrap; serialization/bootstrap/preferred-seeds regression test.
- `crates/radicle/src/node.rs` and `node/command.rs`: bounded disconnect command,
  disposition and control-handle interface used by the chain manager.
- `crates/radicle-protocol/src/service.rs` and `service/command.rs`: disconnect
  acknowledgement and disposition at the service boundary.
- `crates/radicle-node/src/control.rs`, `runtime/handle.rs`, `test/handle.rs` and
  `tests.rs`: bounded disconnect requests and matching handle/test implementations.
  The Outbe wrapper retains its runtime limits, private control UDS and read-only
  loopback HTTP status interface.
- `build.rs`: track enclosing Git refs and version environment overrides,
  including linked worktrees, to refresh build revision metadata.
- `crates/radicle-cli/Cargo.toml` and `crates/radicle-surf/Cargo.toml`: existing
  cargo-machete exceptions for indirect dependencies.
- `crates/radicle-cli/examples/git/git-push-canonical-symbolic-ref.md`: remove
  upstream trailing whitespace to satisfy the enclosing repository diff check.
- This provenance file; no additional fork dependency or duplicate runtime.

Chain dependencies and E2E installation use local crate paths. `rad` and
`git-remote-rad` report the enclosing outbe-chain build revision, which E2E checks
along with source fingerprints and executable digests. The standalone lockfile
tracks the selected upstream release; the root lockfile resolves the chain's
existing compatible versions. No old Heartwood Git source is required.

The upstream MSRV is Rust 1.88.0. Its standalone development toolchain file is
retained from the release; chain builds and validation run from the chain root
with its existing Rust 1.96.0 toolchain. No chain toolchain upgrade is required.

## Native repository authority work (P2, incomplete)

The downstream crypto layer adds a validated Ed25519/BLS repository key and fixed
Commonware-compatible detached BLS subjects/envelopes. DID, COB actor IDs, Git
namespaces, reference caches and fetch results use repository keys; Noise and
transport identities remain Ed25519. Native BLS commit metadata has an exact-byte
codec. Repository namespace announcements negotiate V2 on the authenticated V1
control stream and retain their signed encoding through relay/database reopen.
Migration 10 adds the announcement encoding version; existing rows keep V1.

`radicle::canonical::formatter::CanonicalFormatter` is public so Outbe publication
manifests reuse the same canonical JSON implementation as native identity docs.
Heartwood does not interpret Outbe Vote or checkpoint data.

Native identity/COB/sigrefs attachment and verification, generic publication-policy
integration and full two-node BLS acceptance are still pending. These changes do
not yet establish complete P2 support or any Outbe Vote integration.

The downstream Project description limit is256 UTF-8 bytes (user-directed D3
correction), enforced by constructor/update/deserialization. Name and branch
limits remain255 bytes. Unpatched upstream readers reject a256-byte description;
existing payloads of at most255 bytes retain their encoding.
