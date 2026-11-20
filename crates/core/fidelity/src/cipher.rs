//! `FID2` cohort envelopes. Each transition derives an independent key.

use alloy_primitives::Address;
use outbe_core_common::confidential_tee::{TeeError, FIDELITY_COHORT_ENVELOPE};
use outbe_tee::resident::StateKeyHandle;
use zeroize::Zeroizing;

/// Predecessor, plaintext, and account bound into one cohort ciphertext.
pub struct CohortTransition<'a> {
    /// Account the blob belongs to.
    pub account: Address,
    /// Version already stored. Zero for a first write.
    pub previous_version: u64,
    /// Ciphertext this transition replaces.
    pub previous_blob: &'a [u8],
    /// Padded plaintext interior.
    pub padded_state: &'a [u8],
}

/// Open a cohort blob. An empty blob is version 0 and an empty interior.
pub fn read_blob(
    view_key: &StateKeyHandle,
    account: Address,
    blob: &[u8],
) -> Result<(u64, Vec<u8>), TeeError> {
    if blob.is_empty() {
        return Ok((0, Vec::new()));
    }
    FIDELITY_COHORT_ENVELOPE.read_envelope(view_key, account, &[], blob)
}

/// Seal `transition.padded_state`. `previous_version == u64::MAX` fails closed.
pub fn write_blob(
    view_key: &StateKeyHandle,
    transition: CohortTransition<'_>,
) -> Result<Vec<u8>, TeeError> {
    let previous_len =
        u64::try_from(transition.previous_blob.len()).map_err(|_| TeeError::EncryptFailed(None))?;
    let state_len =
        u64::try_from(transition.padded_state.len()).map_err(|_| TeeError::EncryptFailed(None))?;
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
    FIDELITY_COHORT_ENVELOPE.write_envelope(
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
        let key = StateKeyHandle::for_test([9; 32]);
        let account = Address::repeat_byte(3);
        let seal = |previous_blob: &[u8], padded_state: &[u8]| {
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
        assert!(read_blob(&StateKeyHandle::for_test([8; 32]), account, &first).is_err());
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
                padded_state: b"state",
            },
        )
        .is_err());
    }
}
