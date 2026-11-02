//! Rollback-safe mutable Gratis balance envelopes (`GRA2`), readable with view keys.
use crate::{
    confidential::{BalanceTransition, EnvelopeCipherSpec},
    errors::Result,
};
use alloy_primitives::{Address, U256};

pub const GRATIS_SPEC: EnvelopeCipherSpec = EnvelopeCipherSpec {
    marker: b"GRA2",
    binding_tag: b"outbe/gratis/transition-binding/v2",
    key_info: b"outbe/gratis/amount-key/v2",
    nonce_info: b"outbe/gratis/amount-nonce/v2",
};

pub fn read_amount(
    key: &[u8; 32],
    account: Address,
    field: u8,
    blob: &[u8],
) -> Result<(u64, U256)> {
    GRATIS_SPEC.read_amount(key, account, field, blob)
}

pub fn write_amount(key: &[u8; 32], transition: BalanceTransition) -> Result<Vec<u8>> {
    GRATIS_SPEC.write_amount(key, transition)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::B256;

    #[test]
    fn divergence_and_version_exhaustion_are_safe() {
        let key = [9; 32];
        let account = Address::repeat_byte(3);
        let a = write_amount(
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
        let b = write_amount(
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
        assert_ne!(&a[12..44], &b[12..44]);
        assert_eq!(
            a,
            write_amount(
                &key,
                BalanceTransition {
                    account,
                    field: 0,
                    previous_version: 1,
                    amount: U256::from(10),
                    input_hash: B256::repeat_byte(4)
                }
            )
            .unwrap()
        );
        assert_eq!(
            read_amount(&key, account, 0, &a).unwrap(),
            (2, U256::from(10))
        );
        assert!(read_amount(&key, Address::repeat_byte(7), 0, &a).is_err());
        assert!(write_amount(
            &key,
            BalanceTransition {
                account,
                field: 0,
                previous_version: u64::MAX,
                amount: U256::ONE,
                input_hash: B256::ZERO
            }
        )
        .is_err());
    }
}
