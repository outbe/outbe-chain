//! Submitted oracle prices and volumes stay inside a market-sized bound.

use super::common::{coen_iso, register_voter, usd, with_coen840_oracle, COEN, COEN_ISO_SCALE};

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
    with_coen840_oracle(|storage, oracle, _pair| {
        let validator = register_voter(&storage);

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
