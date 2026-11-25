//! Checks of the other validators for the report of the liveness call.
//!
//! The relayer role reads the signed checkpoints of the validators when it
//! delivers a message. Here it reads them one time in each epoch for the
//! report: a validator that does not serve its checkpoint goes to
//! `notServing`.

use std::sync::Arc;

use alloy_primitives::{Address, B256};
use eyre::{eyre, Result};
use futures::future::join_all;

use crate::relayer::deliver::{announced, Chains};
use crate::relayer::fetch::{Fetcher, Want};

/// The most addresses in the two lists of one report. A free
/// `submitCheckpoint` has 1024 bytes of calldata at most (ZeroFee): the call
/// with an empty report has 484 bytes, and each address adds 32 bytes.
pub const MAX_REPORT_ADDRESSES: usize = 16;

/// One other active validator.
#[derive(Clone, Copy, Debug)]
pub struct OtherValidator {
    /// The validator address `V`.
    pub validator: Address,
    /// The key that signs its checkpoints: `hyperlaneSigner(V)`, or `V`.
    pub signer: Address,
}

/// What the report checks need from the relayer role.
pub struct ReportChecks {
    pub chains: Chains,
    pub fetcher: Arc<Fetcher>,
}

impl ReportChecks {
    /// The validators of `others` that do not serve the checkpoint at `index`
    /// of `domain` with the root `root`. The result is in address order.
    ///
    /// An error means that the check did not run: the origin chain did not
    /// answer, or no other validator answers. When none answers, the cause can be
    /// the network of this agent, so the agent does not name them all.
    pub async fn not_serving(
        &self,
        others: &[OtherValidator],
        domain: u32,
        index: u32,
        message_id: B256,
        root: B256,
    ) -> Result<Vec<Address>> {
        let chain = self
            .chains
            .get(domain)
            .ok_or_else(|| eyre!("domain {domain} is not running"))?;
        let want = Want {
            domain,
            hook: chain.origin.hook,
            index,
            message_id,
        };
        let signers: Vec<Address> = others.iter().map(|other| other.signer).collect();
        let locations =
            announced(chain.provider(), chain.origin.validator_announce, &signers).await?;
        let serves =
            join_all(others.iter().zip(&locations).map(|(other, locations)| {
                self.fetcher.serves(other.signer, locations, &want, root)
            }))
            .await;
        let mut bad: Vec<Address> = others
            .iter()
            .zip(serves)
            .filter(|(_, serves)| !serves)
            .map(|(other, _)| other.validator)
            .collect();
        if others.len() > 1 && bad.len() == others.len() {
            return Err(eyre!(
                "no validator serves the checkpoint at index {index} of domain {domain}"
            ));
        }
        bad.sort_unstable();
        Ok(bad)
    }
}

/// Cuts the two lists of a report to [`MAX_REPORT_ADDRESSES`] together. Each
/// list is in address order, so all agents keep the same addresses.
pub fn fit_report(not_serving: &mut Vec<Address>, not_delivered: &mut Vec<Address>) {
    not_serving.truncate(MAX_REPORT_ADDRESSES);
    not_delivered.truncate(MAX_REPORT_ADDRESSES - not_serving.len());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_report_has_sixteen_addresses_at_most() {
        let addresses = |n: u8| (0..n).map(Address::repeat_byte).collect::<Vec<_>>();
        let (mut serving, mut delivered) = (addresses(10), addresses(10));
        fit_report(&mut serving, &mut delivered);
        assert_eq!((serving.len(), delivered.len()), (10, 6));
        let (mut serving, mut delivered) = (addresses(20), addresses(3));
        fit_report(&mut serving, &mut delivered);
        assert_eq!((serving.len(), delivered.len()), (16, 0));
    }
}
