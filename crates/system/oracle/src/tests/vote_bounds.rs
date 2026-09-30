//! Submitted oracle prices and volumes stay inside a market-sized bound.

use alloy_primitives::Address;

use super::common::{
    coen_iso, init_oracle, native_coen, register_validator, usd, with_storage, COEN, COEN_ISO_SCALE,
};
use crate::schema::OracleContract;

#[test]
fn submit_vote_accepts_the_price_and_volume_ceiling() {
    with_storage(|storage| {
        let mut oracle = OracleContract::new(storage.clone());
        init_oracle(&mut oracle);
        oracle
            .register_pair(crate::types::AddressPair::new_coen_to(840))
            .unwrap();
        let validator = Address::new([0x11; 20]);
        register_validator(storage, validator, native_coen(100));

        oracle
            .submit_vote(
                validator,
                &[(
                    COEN,
                    usd(),
                    coen_iso(1_000_000),
                    coen_iso(1_000_000_000_000),
                )],
            )
            .expect("a price of 1_000_000 and a volume of 10^12 are inside the ceiling");
        assert!(oracle.vote_exists.read(&validator).unwrap());
    });
}

#[test]
fn submit_vote_rejects_a_price_above_one_million() {
    with_storage(|storage| {
        let mut oracle = OracleContract::new(storage.clone());
        init_oracle(&mut oracle);
        oracle
            .register_pair(crate::types::AddressPair::new_coen_to(840))
            .unwrap();
        let validator = Address::new([0x11; 20]);
        register_validator(storage, validator, native_coen(100));

        let err = oracle
            .submit_vote(
                validator,
                &[(COEN, usd(), coen_iso(1_000_001), COEN_ISO_SCALE)],
            )
            .expect_err("a whole price of 1_000_001 is above the ceiling");
        assert!(
            err.to_string().contains("vote price"),
            "unexpected error: {err}"
        );
        assert!(!oracle.vote_exists.read(&validator).unwrap());
    });
}

#[test]
fn submit_vote_rejects_a_volume_above_one_trillion() {
    with_storage(|storage| {
        let mut oracle = OracleContract::new(storage.clone());
        init_oracle(&mut oracle);
        oracle
            .register_pair(crate::types::AddressPair::new_coen_to(840))
            .unwrap();
        let validator = Address::new([0x11; 20]);
        register_validator(storage, validator, native_coen(100));

        let err = oracle
            .submit_vote(
                validator,
                &[(COEN, usd(), coen_iso(1), coen_iso(1_000_000_000_001))],
            )
            .expect_err("a whole volume of 10^12 + 1 is above the ceiling");
        assert!(
            err.to_string().contains("vote volume"),
            "unexpected error: {err}"
        );
        assert!(!oracle.vote_exists.read(&validator).unwrap());
    });
}
