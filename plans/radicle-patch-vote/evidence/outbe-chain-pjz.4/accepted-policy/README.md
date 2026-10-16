# Intermediate P2/P3: Outbe policy for exact accepted Review entries

Frozen source: `275c59a615697d4da86e05eb66e8c12bae9839c4`; component base:
`3ffebe3d4814165afacee024c8537acaf123a534`.
checks.json records exact source/spec hashes, changed files, argv/cwd/environment,
toolchain/platform, start/end times, exits and log hashes. All 53 adapter lib tests,
seven consumer all-target checks, adapter Clippy with `-D warnings`, and formatting
of all three changed Rust files passed. Source was clean after checks.

| Required fragment | Production source | Behavioral evidence in full-libs.log |
| --- | --- | --- |
| Heartwood receives exact accepted OID/context/receipt from Outbe | publication/policy.rs::AcceptedRepositoryPolicy, accept_review | exact_vote_revision_preparation_ignores_later_author_updates_and_threshold_accepts builds full sealed accepted proofs for both real q subsets; only then registers Review COB/sigrefs grants |
| Existing native direct reads deny missing proof and allow exact accepted objects | existing native change::Storage::load and SignedRefs::load_at call PublicationPolicy | separate real Git storage contains the complete object set; before grant both native reads fail; afterward actual Review and complete signed refs load correctly |
| Ordinary Ed author entry is available before owner approval | existing native registry bypass for ordinary author entries | same separate storage reads the exact Ed revision before any policy grant |
| No authority expansion by OID, kind, repo or key | authorize_entry exact request matching; private proof fields | unknown identity OID, wrong subject kinds, another repo and Ed key requests fail |
| History persists across material change without creating new signing permission | accepted proof/current signing separation | scenario first activates providers to version 8; current old signing fails, exact accepted version-7 proof still authorizes those two immutable entries |
| Conflicting trusted inputs do not partially alter grants | accept_review validates scope, sequence receipt hash, finalized observation hash and collisions under one write lock before mutation | synthetic sealed-fixture conflicting block hash, conflicting same-sequence receipt, and another descriptor scope reject; original grant/context/receipt remain intact; duplicate registration and both witness subsets are idempotent |

The policy pins chain/genesis/repository/sole BLS owner/descriptor from a trusted
checkpoint. It accepts no raw peer grants, deserialized acceptance token, arbitrary
context or current authority substitution. Only AcceptedReviewPublication produced
by full native reconstruction and exact receipt/witness verification can populate
the two Review grants. Heartwood keeps its existing generic interface and knows
nothing about Vote or validator sets. No vendor source changed in this component.

The index authorizes archive entries. It does not install refs, grant bootstrap
identity authority, establish that an accepted snapshot is the current latest, or
implement canonical promotion/CAS. The native scenario explicitly asserts that the
receiver still has no refs. Production startup/UDS wiring, persistence/rebuild of
the index, bootstrap/Merge proofs, full collector and multi-validator E2E remain
required. No task closure or acceptance reviewer PASS is claimed.

The first new scenario failed before reaching policy assertions because its Alias
contained whitespace. failed-alias preserves its exact candidate patch, untracked
policy source, base SHA, command, exit 101 and output. This fixture error was corrected;
the frozen source passes the full checks above. It is not a demonstrated production
policy regression. Chain receipts/finality and the conflicting observations in this
scenario are sealed test fixtures, not live executed checkpoints or actual forks.
