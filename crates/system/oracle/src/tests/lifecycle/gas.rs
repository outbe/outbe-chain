use super::*;

const BSC: u32 = 56;

fn gas_price(oracle: &OracleContract, chain_id: u32) -> (U256, u64) {
    (
        oracle.gas_price.read(&chain_id).unwrap(),
        oracle.gas_price_timestamp.read(&chain_id).unwrap(),
    )
}

/// Each voter votes the same pair price and its gas price for BSC.
fn vote_gas(oracle: &mut OracleContract, pair: AddressPair, voters: &[Address], gas: &[u64]) {
    for (voter, gas) in voters.iter().zip(gas) {
        oracle
            .submit_vote(
                *voter,
                &[pair_vote(pair, coen_iso(2), COEN_ISO_SCALE)],
                &[(BSC, U256::from(*gas))],
            )
            .unwrap();
    }
}

#[test]
fn the_tally_writes_the_median_gas_price_at_quorum() {
    with_coen840_oracle(|storage, oracle, pair| {
        oracle.gas_chains.push(BSC).unwrap();
        let voters = register_four_voters(&storage);
        vote_gas(oracle, pair, &voters, &[100, 105, 110, 0]);

        crate::tally::run_tally(oracle, 2, 24).unwrap();

        assert_eq!(gas_price(oracle, BSC), (U256::from(105), 24));
    });
}

#[test]
fn below_quorum_the_tally_keeps_the_last_gas_price() {
    with_coen840_oracle(|storage, oracle, pair| {
        oracle.gas_chains.push(BSC).unwrap();
        oracle.gas_price.write(&BSC, U256::from(7)).unwrap();
        let voters = register_four_voters(&storage);
        vote_gas(oracle, pair, &voters[..2], &[50, 50]);

        crate::tally::run_tally(oracle, 2, 24).unwrap();

        assert_eq!(gas_price(oracle, BSC), (U256::from(7), 0));
    });
}

#[test]
fn a_gas_price_outside_the_band_or_zero_misses() {
    with_coen840_oracle(|storage, oracle, pair| {
        oracle.gas_chains.push(BSC).unwrap();
        let voters = register_four_voters(&storage);
        // Median 100. The spread is the larger of the standard deviation
        // (about 58) and 10% of the median, so 200 and 0 miss.
        vote_gas(oracle, pair, &voters, &[100, 100, 200, 0]);

        crate::tally::run_tally(oracle, 2, 24).unwrap();

        assert_penalty_counters(
            oracle,
            &[
                (voters[0], Penalty::Success, 1),
                (voters[2], Penalty::Miss, 1),
                (voters[3], Penalty::Miss, 1),
            ],
        );
    });
}

#[test]
fn a_vote_for_a_chain_that_is_no_gas_vote_target_fails() {
    with_coen840_oracle(|storage, oracle, pair| {
        let [voter, ..] = register_four_voters(&storage);

        let result = oracle.submit_vote(
            voter,
            &[pair_vote(pair, coen_iso(2), COEN_ISO_SCALE)],
            &[(BSC, U256::from(10))],
        );

        assert!(result.is_err());
        assert!(!oracle.vote_exists.read(&voter).unwrap());
    });
}
