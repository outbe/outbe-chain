//! Targets of a liveness round: the target index of each domain and its
//! delivery index.

use alloy_primitives::{keccak256, Address};
use outbe_primitives::error::Result;

use crate::precompile::IHyperlaneController;
use crate::schema::{validator_domain_key, HyperlaneControllerContract};

/// Reference index of one domain in a liveness round. It is `None` when too
/// few submissions exist for that domain.
pub(crate) type DomainReference = (u32, Option<u32>);

impl HyperlaneControllerContract<'_> {
    /// Moves the target of each domain up to its reference index. Then it
    /// selects the delivery index of the domain: one of the indexes that the
    /// target moved over. The block number and the block time of the verdict
    /// are in the seed, so a validator cannot know the index when it
    /// delivers the message. When the target does not move, there is no
    /// delivery index.
    pub(crate) fn advance_targets(&mut self, references: &[DomainReference]) -> Result<()> {
        for (domain, reference) in references {
            let old = self.target_index.read(domain)?;
            let delivery = match reference.filter(|index| *index > old) {
                Some(index) => {
                    self.target_index.write(domain, index)?;
                    self.emit(IHyperlaneController::TargetIndexSet {
                        domain: *domain,
                        index,
                    })?;
                    let seed = self.delivery_seed(*domain, index)?;
                    // The result is below `index - old`, so it fits `u32`.
                    old + 1 + (seed % u64::from(index - old)) as u32
                }
                None => 0,
            };
            if self.delivery_index.read(domain)? != delivery {
                self.delivery_index.write(domain, delivery)?;
                self.emit(IHyperlaneController::DeliveryIndexSet {
                    domain: *domain,
                    index: delivery,
                })?;
            }
        }
        Ok(())
    }

    /// The seed of the delivery index of `domain` for the new `target`.
    fn delivery_seed(&self, domain: u32, target: u32) -> Result<u64> {
        let mut bytes = [0u8; 48];
        bytes[..8].copy_from_slice(&self.storage.block_number()?.to_be_bytes());
        bytes[8..40].copy_from_slice(&self.storage.timestamp()?.to_be_bytes::<32>());
        bytes[40..44].copy_from_slice(&domain.to_be_bytes());
        bytes[44..].copy_from_slice(&target.to_be_bytes());
        let hash = keccak256(bytes);
        let mut seed = [0u8; 8];
        seed.copy_from_slice(&hash[24..]);
        Ok(u64::from_be_bytes(seed))
    }

    /// The reference index of every domain: the `threshold`-th highest
    /// latest index among `active`.
    pub(crate) fn reference_indexes(
        &self,
        active: &[Address],
        threshold: usize,
    ) -> Result<Vec<DomainReference>> {
        let domains = self.domains.read_all()?;
        let mut references = Vec::with_capacity(domains.len());
        for domain in domains {
            let mut latest = self.latest_indexes(active, domain)?;
            latest.sort_unstable_by(|a, b| b.cmp(a));
            references.push((domain, latest.get(threshold - 1).copied()));
        }
        Ok(references)
    }

    /// The latest index on `domain` of each validator of `active` that the
    /// verdict already evaluates.
    fn latest_indexes(&self, active: &[Address], domain: u32) -> Result<Vec<u32>> {
        let mut latest = Vec::with_capacity(active.len());
        for validator in active {
            let key = validator_domain_key(*validator, domain);
            if self.submitted_block.read(&key)? != 0 {
                latest.push(self.submitted_index.read(&key)?);
            }
        }
        Ok(latest)
    }
}
