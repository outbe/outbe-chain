//! Base purpose tags, shared by every circuit.
//!
//! A tag names *what* is being hashed (a commitment, a nullifier, ...); a
//! domain names *whose* it is (`EMIT_DOMAIN`, `DEMO_TRIBUTE_DOMAIN`, ...). The
//! value actually fed to `hash_multi` is the two combined:
//!
//! ```noir
//! let tag = tags::tag(EMIT_DOMAIN, tags::TAG_COMMITMENT);
//! let commitment = hash_multi(tag, [...]);
//! ```

use crate::hash::hash_2;

// Big-endian ASCII purpose tags.
pub global TAG_COMMITMENT: Field = 0x434f4d4d49544d454e54; // "COMMITMENT"
pub global TAG_NULLIFIER: Field = 0x4e554c4c4946494552; // "NULLIFIER"
pub global TAG_NOTE_SN: Field = 0x4e4f54455f534e; // "NOTE_SN"
pub global TAG_CHANGE_KEY: Field = 0x4348414e47455f4b4559; // "CHANGE_KEY"
pub global TAG_EMPTY: Field = 0x454d505459; // "EMPTY"

/// Domain-separated tag: `Poseidon2(domain, base_tag)`.
pub fn tag(domain: Field, base: Field) -> Field {
    hash_2([domain, base])
}

#[test]
fn base_tags_are_pairwise_distinct_and_nonzero() {
    let tags = [TAG_COMMITMENT, TAG_NULLIFIER, TAG_NOTE_SN, TAG_CHANGE_KEY, TAG_EMPTY];
    for i in 0..tags.len() {
        assert(tags[i] != 0);
        for j in (i + 1)..tags.len() {
            assert(tags[i] != tags[j]);
        }
    }
}

/// Same base tag under two domains must not collide -- the whole point.
#[test]
fn domains_separate_the_same_base_tag() {
    assert(tag(1, TAG_COMMITMENT) != tag(2, TAG_COMMITMENT));
    assert(tag(1, TAG_COMMITMENT) != tag(1, TAG_NULLIFIER));
}
