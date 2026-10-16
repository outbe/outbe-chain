# D3: genesis description bound does not fit native Project

Status: decision proposal only. Dependent bootstrap implementation is stopped.
No production/spec bound was changed. Three independent decision checks and explicit
user approval are required before implementation. This is unrelated to D1/D2.

## Concrete problem

Approved design.md section5 permits a governments description of at most1024 UTF-8
bytes. The OGD1 Rust encoder/decoder and Python genesis seeder enforce that bound.
Native Heartwood Project::new and its deserializer permit only255 bytes. Bootstrap
must construct the canonical native project payload from the literal genesis
description. A256-byte description therefore passes genesis validation but cannot
be used to create governments. This is an actual accepted-input mismatch, not a
claim that the current short fixture description already fails.

Source anchors:
- plans/radicle-patch-vote/design.md section5, description<=1024 literal bytes.
- crates/blockchain/primitives/src/radicle_genesis.rs, MAX_GOVERNMENTS_DESCRIPTION_BYTES=1024, MAX_GOVERNMENTS_DESCRIPTOR_BYTES=1121.
- scripts/genesis_seed_alloc.py::seed_governments_descriptor, description_bytes limit1024.
- vendor/heartwood/crates/radicle/src/identity/project.rs::Project::new and ProjectVisitor; description<=doc::MAX_STRING_LENGTH=255.
- crates/blockchain/radicle/src/integration/bootstrap_probe.rs is a diagnostic calling the actual production encoders/decoder and native constructor at0/255/256/1024/1025 bytes. A successful diagnostic means the mismatch reproduced, not that bootstrap works.

## Proposed decision A (requires user approval)

Align governments genesis description to the native maximum: **255 UTF-8 bytes**.
Reject longer descriptions at genesis preparation/decoding/startup. Preserve valid
description bytes literally. Do not truncate, relocate the description, change RID
construction, or change Heartwood native/Ed25519 rules.

Why propose A: this makes the accepted genesis inputs implementable using the
existing native identity schema and preserves the pinned Heartwood behavior. It
is a contract correction to the approved1024 bound, so it cannot be done silently.
It is not authorization to change a deployed genesis or its commitments.

After approval, the exact change is:
1. design.md section5 and the current descriptor self-check: maximum255 bytes.
2. radicle_genesis.rs: maximum description255; maximum OGD1 bytes97+255=352. The encoding/version/field layout stays identical for already-supported inputs.
3. genesis_seed_alloc.py: reject description UTF-8 bytes>255, clear error text.
4. Rust/Python boundary tests:255 accepted,256 rejected; multibyte254/255/256 cases; existing invalid CR/NUL/chain/time/hash/oversize/trailing cases preserved. Replace the diagnostic with aligned behavior coverage after the approved correction.
5. New exact-SHA evidence and re-review of every affected closed acceptance scope, especially P0; do not carry old PASS for a relevant changed spec. P2/P3/P4 remain incomplete regardless. Reviewers must identify any additional affected prior acceptance scope rather than assuming all previous PASS survive.

Historical evidence bundles and historical review reports are immutable evidence:
do not rewrite their old1024 assertions or hashes to appear current. Keep the original
source/spec snapshot and add a new correction/evidence/review round.

## Impact and alternative

Descriptors with descriptions0..255 bytes keep exactly the same OGD1 bytes/hash and
RID inputs. Previously allowed256..1024-byte descriptors become invalid under A.
If any such genesis has already been seeded, A is a compatibility change that must
be explicitly assessed by its operator; this task does not mutate that genesis.

Retaining1024 instead would require deliberately widening native Heartwood Project
acceptance and examining native/legacy reader compatibility. That would be a larger
alternative change, not an approved fallback and not implemented here. Simply
bypassing Project::new is insufficient because the native deserializer validates
the same constraint. Vote, BLS algorithms, validator eligibility, managed policy,
Git fetch/storage and collector semantics are unaffected by the proposed A.

No acceptance task is closed by this decision check. Root checkout and the old
untracked D1/D2 directories remain untouched.
