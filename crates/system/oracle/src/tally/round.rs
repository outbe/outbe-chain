//! One tally round over the votes of a vote period.

use alloy_primitives::{Address, U256};
use alloy_sol_types::SolEvent;
use outbe_primitives::address_pair::AddressPair;
use outbe_primitives::addresses::ORACLE_ADDRESS;
use outbe_primitives::error::Result;

use super::ballot::{
    apply_winners, credit_win, evaluate_pair, mark_participation, observation_count, pair_quorum,
    vote_has_price,
};
use super::cross_rate::{from_cross_rate, to_cross_rate};
use super::{Claim, VoteForTally};
use crate::precompile::IOracle;
use crate::schema::{OracleContract, PairIndex};

/// Block that runs the tally, and its timestamp.
#[derive(Clone, Copy, Debug)]
struct TallyClock {
    block_number: u64,
    timestamp: u64,
}

/// Votes of one active vote target.
struct PairBallot {
    /// Registry index that keys the rate columns of the pair.
    index: PairIndex,
    pair: AddressPair,
    votes: Vec<VoteForTally>,
    /// `true` when the ballot reaches the raw pair quorum.
    qualified: bool,
}

/// Validators, claims and ballots of one tally round.
struct TallyRound {
    /// Active validators at tally time.
    validators: Vec<Address>,
    claims: Vec<(Address, Claim)>,
    ballots: Vec<PairBallot>,
}

/// Rates that one tally round publishes.
#[derive(Default)]
struct PublishedRates {
    pairs_updated: u32,
    /// Snapshot entries `(pair, rate, volume)` in publication order.
    snapshot_entries: Vec<(AddressPair, U256, U256)>,
}

/// Runs the round body. [`super::run_tally`] wraps it in a storage checkpoint.
pub(super) fn run_tally_inner(
    oracle: &mut OracleContract,
    block_number: u64,
    timestamp: u64,
) -> Result<()> {
    let enabled = oracle.config_enabled.read()?;
    if !enabled {
        return Ok(());
    }

    let reward_band = oracle.config_reward_band.read()?;
    let Some(mut round) = open_round(oracle)? else {
        return oracle.clear_votes();
    };
    let total_targets = round.ballots.len() as u32;

    // Raw pair quorum is independent per pair.
    round.qualify_ballots();

    let clock = TallyClock {
        block_number,
        timestamp,
    };
    let rates = match round.reference_ballot() {
        Some(reference) => round.publish_rates(oracle, reference, reward_band, clock)?,
        None => PublishedRates::default(),
    };

    // Write price snapshot
    if !rates.snapshot_entries.is_empty() {
        oracle.write_snapshot(timestamp, &rates.snapshot_entries)?;
    }

    // Count miss/success/abstain per validator
    count_outcomes(oracle, &round.claims, total_targets)?;

    // Emit TallyCompleted event
    let event = IOracle::TallyCompleted {
        blockNumber: block_number,
        pairsUpdated: rates.pairs_updated,
    };
    let _ = oracle
        .storage
        .emit_event(ORACLE_ADDRESS, event.encode_log_data());

    // Clear all votes
    oracle.clear_votes()?;

    Ok(())
}

/// Reads the validators, vote targets and votes of the round.
///
/// Returns `None` when the round has no active validator, no vote, or no
/// vote target. The caller then clears the votes. A round without votes
/// first records an abstention for every active validator.
fn open_round(oracle: &mut OracleContract) -> Result<Option<TallyRound>> {
    // Collect the active validator set at tally time.
    // This is an intentional divergence from Cosmos, which locks the set at
    // period start. The tally revalidates membership at tally time, so a
    // validator that exited after submitting cannot contribute to quorum.
    // Snapshotting membership at vote
    // time would require additional storage per period per validator.
    let vs = outbe_validatorset::contract::ValidatorSet::new(oracle.storage.clone());
    let validators: Vec<Address> = vs
        .get_active_validators()?
        .iter()
        .map(|v| v.validator_address)
        .collect();
    if validators.is_empty() {
        return Ok(None);
    }

    let voter_count = oracle.voter_list.len()?;
    if voter_count == 0 {
        // No votes this period - all validators get abstain
        for validator in &validators {
            oracle.increment_abstain(validator)?;
        }
        return Ok(None);
    }

    let ballots = active_ballots(oracle)?;
    if ballots.is_empty() {
        return Ok(None);
    }

    let mut round = TallyRound {
        claims: validators
            .iter()
            .map(|validator| (*validator, Claim::default()))
            .collect(),
        validators,
        ballots,
    };
    round.collect_votes(oracle, voter_count)?;
    Ok(Some(round))
}

/// Returns an empty ballot for each vote target (active pair), in registry
/// order. Each ballot keeps the registry index that keys the rate columns, so
/// the write-back never re-derives it from the pair.
fn active_ballots(oracle: &OracleContract) -> Result<Vec<PairBallot>> {
    let pair_count = oracle.pair_count.read()?;
    let mut ballots = Vec::new();
    for pid in 1..=pair_count {
        let pair = oracle.pair_at(pid)?;
        if oracle.vote_target.read(&pair)? {
            ballots.push(PairBallot {
                index: pid,
                pair,
                votes: Vec::new(),
                qualified: false,
            });
        }
    }
    Ok(ballots)
}

impl TallyRound {
    /// Reads every vote of every active validator in voter-list order and puts
    /// it on the ballot of its market. A vote for an inactive market is ignored.
    fn collect_votes(&mut self, oracle: &OracleContract, voter_count: u32) -> Result<()> {
        for vi in 0..voter_count {
            let voter = oracle.voter_list.get(vi)?.unwrap_or(Address::ZERO);
            if !self.validators.contains(&voter) {
                continue;
            }
            self.collect_voter_votes(oracle, voter)?;
        }
        Ok(())
    }

    fn collect_voter_votes(&mut self, oracle: &OracleContract, voter: Address) -> Result<()> {
        let tuple_count = oracle.vote_tuple_count.read(&voter)?;

        let pair_map = oracle.vote_pair.get_nested(&voter);
        let rate_map = oracle.vote_rate.get_nested(&voter);
        let volume_map = oracle.vote_volume.get_nested(&voter);

        for ti in 0..tuple_count {
            let voted_pair = pair_map.read_pair(&ti)?;
            let rate = rate_map.read(&ti)?;
            let volume = volume_map.read(&ti)?;

            // Find the ballot for this pair
            if let Some(ballot) = self
                .ballots
                .iter_mut()
                .find(|ballot| ballot.pair.same_market(&voted_pair))
            {
                mark_participation(&mut self.claims, voter);
                ballot.votes.push(VoteForTally {
                    exchange_rate: rate,
                    volume,
                    voter,
                });
            }
        }
        Ok(())
    }

    /// Marks each ballot that reaches the raw pair quorum as qualified.
    fn qualify_ballots(&mut self) {
        let quorum = pair_quorum(self.validators.len());
        for ballot in &mut self.ballots {
            ballot.qualified = observation_count(&ballot.votes) >= quorum;
            if !ballot.qualified {
                // Cosmos-style participation credit: the tally does not punish a valid
                // observation on a pair that lacks quorum as an outlier. Missing and
                // zero-rate observations receive no credit for that pair.
                for vote in ballot.votes.iter().filter(|vote| vote_has_price(vote)) {
                    credit_win(&mut self.claims, vote.voter);
                }
            }
        }
    }

    /// Returns the qualified ballot with the most validator observations.
    /// Iteration follows registry order, so equal counts keep the first pair.
    fn reference_ballot(&self) -> Option<usize> {
        let mut current = self.ballots.iter().position(|ballot| ballot.qualified)?;
        for index in (current + 1)..self.ballots.len() {
            if self.ballots[index].qualified
                && observation_count(&self.ballots[index].votes)
                    > observation_count(&self.ballots[current].votes)
            {
                current = index;
            }
        }
        Some(current)
    }

    /// Tallies the reference ballot directly and every other qualified ballot
    /// through the reference overlap. Publishes each nonzero result.
    fn publish_rates(
        &mut self,
        oracle: &mut OracleContract,
        reference: usize,
        reward_band: U256,
        clock: TallyClock,
    ) -> Result<PublishedRates> {
        let mut rates = PublishedRates::default();

        // Tally reference pair directly.
        let reference_ballot = &self.ballots[reference];
        let ref_median = evaluate_pair(&reference_ballot.votes, reward_band)?;

        let reference_votes: Vec<(Address, U256)> = reference_ballot
            .votes
            .iter()
            .filter(|vote| vote_has_price(vote))
            .map(|v| (v.voter, v.exchange_rate))
            .collect();

        if !ref_median.median.is_zero() {
            apply_winners(&mut self.claims, &ref_median.winning_validators);
            rates.publish(
                oracle,
                clock,
                reference_ballot,
                ref_median.median,
                ref_median.volume,
            )?;
        }

        // Tally every other quorum-qualified pair via the reference overlap.
        // There is intentionally no second quorum over that intersection.
        let ref_pair = reference_ballot.pair;
        for (i, ballot) in self.ballots.iter().enumerate() {
            if i == reference || !ballot.qualified {
                continue;
            }

            let cross_ballot =
                to_cross_rate(&ballot.votes, &reference_votes, ref_pair, ballot.pair)?;
            let cross_outcome = evaluate_pair(&cross_ballot, reward_band)?;
            if cross_outcome.median.is_zero() {
                continue;
            }
            let Some(actual_rate) = from_cross_rate(
                ref_median.median,
                cross_outcome.median,
                ref_pair,
                ballot.pair,
            ) else {
                continue;
            };

            apply_winners(&mut self.claims, &cross_outcome.winning_validators);
            rates.publish(oracle, clock, ballot, actual_rate, cross_outcome.volume)?;
        }
        Ok(rates)
    }
}

impl PublishedRates {
    /// Stores `rate` for the pair of `ballot` and emits `ExchangeRateUpdated`.
    /// Then queues a snapshot entry when the snapshot can accept it.
    fn publish(
        &mut self,
        oracle: &mut OracleContract,
        clock: TallyClock,
        ballot: &PairBallot,
        rate: U256,
        volume: U256,
    ) -> Result<()> {
        let pair = ballot.pair;
        oracle.update_exchange_rate(ballot.index, rate, clock.block_number, clock.timestamp)?;
        self.pairs_updated += 1;
        let event = IOracle::ExchangeRateUpdated {
            base: pair.address1(),
            quote: pair.address2(),
            rate,
            blockNumber: clock.block_number,
        };
        let _ = oracle
            .storage
            .emit_event(ORACLE_ADDRESS, event.encode_log_data());

        if oracle.snapshot_can_accept(clock.timestamp, pair, rate, volume)? {
            self.snapshot_entries.push((pair, rate, volume));
        }
        Ok(())
    }
}

/// Records success, abstain or miss for every claim. A success needs a win on
/// every vote target.
fn count_outcomes(
    oracle: &mut OracleContract,
    claims: &[(Address, Claim)],
    total_targets: u32,
) -> Result<()> {
    for (addr, claim) in claims {
        if claim.win_count == total_targets {
            oracle.increment_success(addr)?;
        } else if !claim.did_vote {
            oracle.increment_abstain(addr)?;
        } else {
            oracle.increment_miss(addr)?;
        }
    }
    Ok(())
}
