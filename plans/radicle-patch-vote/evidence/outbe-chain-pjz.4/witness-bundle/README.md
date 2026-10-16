# Intermediate P3/P6: separate bounded witness bundle

Frozen source: `9352d953838866f5a9cdcfebdb7119d933d6601a`. Exact commands,
source/spec hashes, cwd/environment/toolchain/platform, times/exits and log hashes
are in `checks.json`. All 49 adapter tests, seven consumers all-target checks,
adapter Clippy -D warnings and scoped rustfmt passed. No full task acceptance.

| Contract fragment | Source | Behavioral evidence in full-libs.log |
| --- | --- | --- |
| Witness evidence is separate from stable native manifest | `integration/signing/witness.rs::RepositoryWitnessBundle` | native Review scenario roundtrips both 012 and 123 bundles; different witness bytes independently verify the same actual COB/refs envelopes; existing native OID/manifest equality assertions remain |
| One MiB total wire cap before decoding; canonical encoding and bounded subject/participant counts | `RepositoryWitnessBundle::{from_bytes,to_bytes,new}` | `assert_witness_bundle`: exact-one-MiB structural fixture accepted, limit+1 rejected; encoder overflow, >3 subjects, >256 frames, duplicate/gapped ordinals, duplicate/unsorted index lists rejected |
| Exact public Output and every native subject must match trusted material and reconstructed subjects | `RepositoryWitnessBundle::verify`, existing `RepositorySigningMaterial::verify_witness` | changed public Output, wrong subject count/order and q-1 frames fail; each decoded native bundle verifies against independent public-only material |
| Strict canonical JSON/private decoded representation | private `BundleWire`/`SubjectWire` | unknown top-level/nested fields, wrong version, duplicate keys, trailing whitespace/truncation, malformed/truncated frame hex, uppercase hex, empty Output/participant lists rejected |

The adapter wire representation is canonical JSON using Heartwood's existing
CanonicalFormatter, with version=1, publicOutput=lower-case hex of canonical SDK
Output, subjects=[{ordinal,partials:[lower-case hex268-byte frames]}]. Subject
ordinals are contiguous native signing order; wire frames must have strictly
increasing participant indices. Typed construction sorts each supplied frame list
and rejects duplicates. The bundle has no Serialize/Deserialize bypass around its
bounded decoder/private representation. It never becomes a native object/ref and
is not included in the native manifest/body hash.

Decoded bundles compare exact Output bytes with authenticated signing material,
then independently verify each subject's current authenticated q frames and recovered
envelope using the public-only verifier. Actual subjects must come from checked
native Git objects; the codec never turns peer-supplied subject bytes into signing
permission. The existing native scenario supplies real SDK partials returned by
local actor bridges for COB/refs. It tests different valid subsets with fixed native
objects and manifest. Checkpoint/authority providers are fixtures, not live consensus.

The exact-size fixture pads an opaque Output field to exercise the codec boundary.
It is structurally valid, cryptographically invalid, and explicitly rejected by
verification against real local material. This is not evidence of a valid one-MiB
DKG output or live maxcommittee threshold verification. Maxcommittee live q-witness
and complete package E2E remain required by P3/P8.

## Remaining work

The bundle codec and cryptographic verifier do not replace complete independent
actual-Git-object/SHA256/native-envelope/package verification, current permission
checks before attestation/publication, protected node/sidecar UDS, distributed
collector/journal, automatic genesis bootstrap, separate Merge or production E2E.
Those remain incomplete. P2/P3 remain in_progress; no task acceptance reviews or
closures were performed. No git.rs/fetch-gate/D2 or native wire format changes.
