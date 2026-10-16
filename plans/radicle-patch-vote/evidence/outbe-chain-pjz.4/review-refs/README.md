# Intermediate P3: threshold signing of native Review refs

Source: `479f8fa02`. Exact full source SHA, parent, specification hashes, commands,
environment, times, exits and log hashes are recorded in `checks.json`.
This evidence does not close P3 or P6.

## Production interface

An attached `PreparedPatchReview` now returns sealed `PreparedReviewRefs`. Its
fields are private; it can only be constructed from the native Review attachment
path. It preserves the exact owner, OPJ1 ordinal-1 context, current signing
context, accepted owner refs and previous sigrefs parent used during preparation.
It exposes the reconstructed native signed-refs subject, never a free message or
caller-selected signing namespace. Attaching its verified signature stores native
objects without promoting any refs.

`RepositorySigningMaterial::{sign_review_refs,verify_review_refs_partial,
recover_review_refs}` use the same private contribution-validation and recovery
implementation as the COB Review methods. They check the prepared context and
owner, identity/address/participant mapping, authenticated fixed-length partial
frame and current full polynomial. The shared versioned provider rechecks active
material under its existing lock. No secret share is exported or reconstructed.

Native Review and refs subjects have different RDB1 kinds and different OPJ1
ordinals. Partial authentication commits the exact native subject hash, so a
valid COB partial cannot be counted for signed refs or vice versa. The threshold
signature is still a native MinSig signature, not an aggregate of independent
validator identity signatures.

## Behavioral evidence

The existing `exact_vote_revision_preparation_ignores_later_author_updates_and_threshold_accepts`
scenario now signs the derived native refs through the production Outbe signer
interface instead of the low-level bootstrap test helper. It uses four actual
threshold share providers with q=3 and authenticated MinPk participant identities.
It covers:

- serialized contribution roundtrip and authenticated sender verification;
- COB/refs cross-use, wrong sender, q-1 and duplicate-index rejection;
- subsets 012 and 123 recovering byte-identical native refs envelopes;
- repeated exact preparation producing the same native refs subject;
- attachment preserving the old `rad/sigrefs` until explicit publication;
- native signed-refs loading with the exact parent and Review entry, unchanged
  main branch, Accept only on the approved revision, and patch still open;
- current provider transition rejecting further sign/verify/recovery for old
  material while the previously recovered envelope remains cryptographically
  valid.

The test's receipt provider and explicit ref promotion remain fixtures. It does
not prove production finalized-ticket admission, collector transport, journal or
two live distributed collectors. Production canonical Output provisioning,
bootstrap signing and separate Merge signing remain incomplete, along with the
remaining approved P3/P4/P6/P7/P8 wiring. No task acceptance review or closure is
claimed. There are no Heartwood, `git.rs`, fetch-gate or D2 source changes.
