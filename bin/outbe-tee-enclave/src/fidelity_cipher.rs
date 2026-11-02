//! Fresh-genesis cohort envelopes (`FID2`) with independent keys for divergent transitions.
use crate::{
    confidential::EnvelopeCipherSpec,
    errors::{Result, TeeError},
};
use alloy_primitives::Address;
use zeroize::Zeroizing;

pub const FIDELITY_SPEC: EnvelopeCipherSpec = EnvelopeCipherSpec {
    marker: b"FID2",
    binding_tag: b"outbe/fidelity/cohort-transition-binding/v2",
    key_info: b"outbe/fidelity/cohort-key/v2",
    nonce_info: b"outbe/fidelity/cohort-nonce/v2",
};

pub struct CohortTransition<'a> {
    pub account: Address,
    pub previous_version: u64,
    pub previous_blob: &'a [u8],
    pub padded_state: &'a [u8],
}

pub fn read_blob(view_key: &[u8; 32], account: Address, blob: &[u8]) -> Result<(u64, Vec<u8>)> {
    FIDELITY_SPEC.read_envelope(view_key, account, &[], blob)
}

pub fn write_blob(view_key: &[u8; 32], transition: CohortTransition<'_>) -> Result<Vec<u8>> {
    let previous_len =
        u64::try_from(transition.previous_blob.len()).map_err(|_| TeeError::EncryptFailed)?;
    let state_len =
        u64::try_from(transition.padded_state.len()).map_err(|_| TeeError::EncryptFailed)?;
    let mut binding_payload = Vec::with_capacity(
        16 + transition
            .previous_blob
            .len()
            .saturating_add(transition.padded_state.len()),
    );
    binding_payload.extend_from_slice(&previous_len.to_be_bytes());
    binding_payload.extend_from_slice(transition.previous_blob);
    binding_payload.extend_from_slice(&state_len.to_be_bytes());
    binding_payload.extend_from_slice(transition.padded_state);
    let binding_payload = Zeroizing::new(binding_payload);
    FIDELITY_SPEC.write_envelope(
        view_key,
        (transition.account, &[]),
        (transition.previous_version, &binding_payload),
        transition.padded_state,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    const HEADER_LEN: usize = 8 + 4 + 32;
    #[test]
    fn envelope_binds_predecessor_account_and_state_and_checks_overflow() {
        let key = [9; 32];
        let account = Address::repeat_byte(3);
        let seal = |previous_blob, padded_state| {
            write_blob(
                &key,
                CohortTransition {
                    account,
                    previous_version: 0,
                    previous_blob,
                    padded_state,
                },
            )
            .unwrap()
        };
        let first = seal(&[], b"first padded state");
        assert_eq!(first, seal(&[], b"first padded state"));
        let divergent = seal(&[], b"other padded state");
        assert_ne!(&first[12..HEADER_LEN], &divergent[12..HEADER_LEN]);
        let (first_key, first_nonce) = FIDELITY_SPEC
            .opening_material(&key, account, &[], (1, &first[12..HEADER_LEN]))
            .unwrap();
        let (other_key, other_nonce) = FIDELITY_SPEC
            .opening_material(&key, account, &[], (1, &divergent[12..HEADER_LEN]))
            .unwrap();
        assert_ne!(*first_key, *other_key);
        assert_ne!(first_nonce, other_nonce);
        let next = |predecessor: &[u8]| {
            write_blob(
                &key,
                CohortTransition {
                    account,
                    previous_version: 1,
                    previous_blob: predecessor,
                    padded_state: b"identical final state",
                },
            )
            .unwrap()
        };
        assert_ne!(
            &next(&first)[12..HEADER_LEN],
            &next(&divergent)[12..HEADER_LEN]
        );
        assert_eq!(
            read_blob(&key, account, &first).unwrap(),
            (1, b"first padded state".to_vec())
        );
        assert!(read_blob(&[8; 32], account, &first).is_err());
        assert!(read_blob(&key, Address::repeat_byte(4), &first).is_err());
        for offset in [0, 8, 12, HEADER_LEN] {
            let mut altered = first.clone();
            altered[offset] ^= 1;
            assert!(read_blob(&key, account, &altered).is_err());
        }
        assert!(read_blob(&key, account, &[0; 56]).is_err());
        assert!(write_blob(
            &key,
            CohortTransition {
                account,
                previous_version: u64::MAX,
                previous_blob: &first,
                padded_state: b"state"
            }
        )
        .is_err());
    }
}
