//! Submitted oracle prices and volumes stay inside a market-sized bound. A
//! tuple outside it is dropped on its own; the vote's other tuples still count.

use super::common::{
    coen_iso, eur, register_voter, usd, with_coen840_oracle, COEN, COEN_ISO_SCALE,
};
use outbe_primitives::address_pair::AddressPair;

#[test]
fn submit_vote_accepts_the_price_and_volume_ceiling() {
    with_coen840_oracle(|storage, oracle, _pair| {
        let validator = register_voter(&storage);

        oracle
            .submit_vote(
                validator,
                &[(
                    COEN,
                    usd(),
                    coen_iso(1_000_000_000),
                    coen_iso(1_000_000_000_000),
                )],
            )
            .expect("a price of 10^9 and a volume of 10^12 are inside the ceiling");
        assert!(oracle.vote_exists.read(&validator).unwrap());
    });
}

#[test]
fn submit_vote_rejects_a_price_above_one_billion() {
    with_coen840_oracle(|storage, oracle, _pair| {
        let validator = register_voter(&storage);

        let err = oracle
            .submit_vote(
                validator,
                &[(COEN, usd(), coen_iso(1_000_000_001), COEN_ISO_SCALE)],
            )
            .expect_err("a whole price of 10^9 + 1 is above the ceiling");
        assert!(
            err.to_string().contains("vote price"),
            "unexpected error: {err}"
        );
        assert!(!oracle.vote_exists.read(&validator).unwrap());
    });
}

#[test]
fn submit_vote_rejects_a_volume_above_one_trillion() {
    with_coen840_oracle(|storage, oracle, _pair| {
        let validator = register_voter(&storage);

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

#[test]
fn submit_vote_keeps_the_in_range_tuples() {
    with_coen840_oracle(|storage, oracle, _pair| {
        oracle.register_pair(AddressPair::new_coen_to(978)).unwrap();
        let validator = register_voter(&storage);

        let (_, admitted) = oracle
            .submit_vote(
                validator,
                &[
                    (COEN, usd(), coen_iso(2), COEN_ISO_SCALE),
                    (COEN, eur(), coen_iso(1_000_000_001), COEN_ISO_SCALE),
                ],
            )
            .unwrap();
        assert_eq!(admitted, 1);

        let (_, _, quotes, rates, _) = oracle.get_aggregate_vote(&validator).unwrap();
        assert_eq!(quotes, vec![usd()]);
        assert_eq!(rates, vec![coen_iso(2)]);
    });
}
