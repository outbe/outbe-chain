//! The ODKO boundary-outcome codec: one encoder, one strict decoder.

use super::*;
use commonware_cryptography::bls12381::primitives::sharing::Mode;
use commonware_cryptography::{bls12381::dkg::feldman_desmedt::deal, Signer as _};
use commonware_utils::{ordered::Set, N3f1};
use rand_commonware::SeedableRng as _;

fn sample(epoch: u64, is_full_dkg: bool) -> OdkoOutcome {
    let participants =
        Set::from_iter_dedup((0..4).map(|i| bls12381::PrivateKey::from_seed(i).public_key()));
    let (output, _) = deal::<MinSig, _, N3f1>(
        rand_commonware::rngs::StdRng::seed_from_u64(0),
        Mode::NonZeroCounter,
        participants,
    )
    .unwrap();
    OdkoOutcome {
        epoch: Epoch::new(epoch),
        is_full_dkg,
        output,
    }
}

#[test]
fn odko_outcome_round_trips() {
    for (epoch, is_full_dkg) in [(0, false), (7, true), (u64::MAX, false)] {
        let outcome = sample(epoch, is_full_dkg);
        assert_eq!(OdkoOutcome::decode(&outcome.encode()), Ok(outcome));
    }
}

#[test]
fn odko_decoder_rejects_non_canonical_records() {
    let encoded = sample(3, true).encode().to_vec();

    let mut bad_flag = encoded.clone();
    bad_flag[13] = 2;
    assert!(
        OdkoOutcome::decode(&bad_flag).is_err(),
        "is_full_dkg must be 0 or 1"
    );

    let mut trailing = encoded.clone();
    trailing.push(0);
    assert!(OdkoOutcome::decode(&trailing).is_err(), "trailing bytes");

    let mut bad_magic = encoded.clone();
    bad_magic[0] = b'X';
    assert!(OdkoOutcome::decode(&bad_magic).is_err(), "magic");

    let mut bad_version = encoded;
    bad_version[4] = 0x01;
    assert!(OdkoOutcome::decode(&bad_version).is_err(), "version");

    assert!(OdkoOutcome::decode(b"ODKO").is_err(), "truncated header");
}

#[test]
fn legacy_helpers_agree_with_the_codec() {
    let outcome = sample(5, false);
    let encoded = outcome.encode();
    assert_eq!(decode_boundary_outcome(&encoded), Some(outcome.output));
}

mod properties {
    use proptest::prelude::*;

    use super::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        /// Checks these properties, with no panics:
        ///
        /// - any epoch/flag round-trips.
        /// - a single-byte header corruption is either rejected or still canonical.
        /// - any truncation is rejected.
        #[test]
        fn round_trips_and_corrupted_headers_are_rejected_or_canonical(
            epoch in any::<u64>(),
            is_full_dkg in any::<bool>(),
            position in 0usize..18,
            value in any::<u8>(),
            truncate in 0usize..32,
        ) {
            let outcome = sample(epoch, is_full_dkg);
            let encoded = outcome.encode().to_vec();
            prop_assert_eq!(OdkoOutcome::decode(&encoded), Ok(outcome.clone()));

            let mut corrupted = encoded.clone();
            corrupted[position] = value;
            // Canonical: whatever still decodes re-encodes to the exact bytes.
            if let Ok(decoded) = OdkoOutcome::decode(&corrupted) {
                prop_assert_eq!(decoded.encode().to_vec(), corrupted);
            }

            let cut = encoded.len().saturating_sub(truncate + 1);
            prop_assert!(OdkoOutcome::decode(&encoded[..cut]).is_err());
        }
    }
}
