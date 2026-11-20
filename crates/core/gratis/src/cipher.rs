//! Rollback-safe mutable Gratis balance envelopes (`GRA2`).

use alloy_primitives::{Address, U256};
use outbe_core_common::confidential_tee::{BalanceTransition, TeeError, GRATIS_BALANCE_ENVELOPE};
use outbe_tee::resident::StateKeyHandle;

/// Open a Gratis amount. An empty blob is version 0 and amount 0.
pub fn read_amount(
    key: &StateKeyHandle,
    account: Address,
    field: u8,
    blob: &[u8],
) -> Result<(u64, U256), TeeError> {
    GRATIS_BALANCE_ENVELOPE.read_amount(key, account, field, blob)
}

/// Seal the next Gratis amount for `transition`.
pub fn write_amount(
    key: &StateKeyHandle,
    transition: BalanceTransition,
) -> Result<Vec<u8>, TeeError> {
    GRATIS_BALANCE_ENVELOPE.write_amount(key, transition)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::B256;

    #[test]
    fn divergence_and_version_exhaustion_are_safe() {
        let key = StateKeyHandle::for_test([9; 32]);
        let account = Address::repeat_byte(3);
        let first = write_amount(
            &key,
            BalanceTransition {
                account,
                field: 0,
                previous_version: 1,
                amount: U256::from(10),
                input_hash: B256::repeat_byte(4),
            },
        )
        .unwrap();
        let second = write_amount(
            &key,
            BalanceTransition {
                account,
                field: 0,
                previous_version: 1,
                amount: U256::from(11),
                input_hash: B256::repeat_byte(5),
            },
        )
        .unwrap();
        assert_ne!(&first[12..44], &second[12..44]);
        assert_eq!(
            first,
            write_amount(
                &key,
                BalanceTransition {
                    account,
                    field: 0,
                    previous_version: 1,
                    amount: U256::from(10),
                    input_hash: B256::repeat_byte(4),
                },
            )
            .unwrap()
        );
        assert_eq!(
            read_amount(&key, account, 0, &first).unwrap(),
            (2, U256::from(10))
        );
        assert!(read_amount(&key, Address::repeat_byte(7), 0, &first).is_err());
        assert!(write_amount(
            &key,
            BalanceTransition {
                account,
                field: 0,
                previous_version: u64::MAX,
                amount: U256::ONE,
                input_hash: B256::ZERO,
            },
        )
        .is_err());
    }
}
