# D3 user decision:256 UTF-8 bytes

User instruction after the limit explanation: `нужпо поставить 256`.
This explicitly chooses256 rather than the earlier proposed255. The old proposal
and its3 decision-review reports remain historical and are not approval of this variant.

Apply description maximum256 to OGD1 validation/seeding/design and the downstream
native Project constructor/update/deserializer. Keep name/default branch255,
literal UTF-8/no truncation, OGD1 framing/schema and existing short descriptor hash.
Maximum descriptor size becomes97+256=353. No Vote/BLS/storage/fetch changes.
Unpatched upstream readers reject description256. Existing descriptions257..1024
will be rejected by corrected genesis. No deployed genesis is rewritten or inspected.

P0 design acceptance is reopened for fresh evidence and3 independent full-AC
reviews. P1 upstream migration acceptance and P5 exact-patch Vote behavior are
not changed by the description validation rule; their historical reviews are not
represented as review of this correction. P2/P3 remain incomplete.
