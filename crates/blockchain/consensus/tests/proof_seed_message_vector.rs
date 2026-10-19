//! - byte-pinned VRF seed-message test vectors.
//!
//! The V2 verifier computes the seed message as `Round(epoch, view).encode()`
//! via `commonware_codec::Encode`. A future commonware bump could silently
//! change this encoding and produce subtly different VRF inputs across the
//! network - a hard consensus split. These tests pin the byte layout against
//! known `(epoch, view)` pairs so any drift fails the build.

use commonware_codec::Encode;
use commonware_consensus::types::{Epoch, Round, View};

#[test]
fn round_encode_test_vector_for_vrf_seed_message() {
    // (epoch=0, view=1) - genesis-adjacent round used by block 1 production.
    // Round encodes via commonware varint: single-byte values < 128 emit one
    // byte directly. So Round(0, 1) -> [0x00, 0x01].
    let r0 = Round::new(Epoch::new(0), View::new(1));
    let bytes0 = r0.encode().to_vec();
    assert_eq!(
        bytes0,
        vec![0x00, 0x01],
        "Round(0, 1).encode() byte-pin: varint epoch=0 || varint view=1"
    );

    // (epoch=12, view=61) - view-61 reproduction round halt.
    // Both fit single-byte varints: 12 = 0x0c, 61 = 0x3d.
    let r1 = Round::new(Epoch::new(12), View::new(61));
    let bytes1 = r1.encode().to_vec();
    assert_eq!(
        bytes1,
        vec![0x0c, 0x3d],
        "Round(12, 61).encode() byte-pin: varint epoch=12 || varint view=61"
    );

    // Determinism across N encode invocations.
    for _ in 0..32 {
        assert_eq!(
            Round::new(Epoch::new(12), View::new(61)).encode().to_vec(),
            bytes1
        );
    }
}
