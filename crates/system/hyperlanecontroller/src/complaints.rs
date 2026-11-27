//! Complaints: what the reports of a liveness round say about the other
//! validators, and the miss reasons that come from them.

use alloy_primitives::{Address, B256};
use outbe_primitives::error::Result;

use crate::liveness::miss_reason;
use crate::schema::{
    complaint_key, complaints_key, validator_domain_key, Fault, HyperlaneControllerContract,
};

impl HyperlaneControllerContract<'_> {
    /// Adds the complaint of `reporter` about each validator of `accused`. A
    /// second complaint of the same reporter about the same validator in one
    /// round does not count.
    pub(crate) fn record_complaints(
        &mut self,
        reporter: Address,
        domain: u32,
        fault: Fault,
        accused: &[Address],
        round: u64,
    ) -> Result<()> {
        for validator in accused {
            let complaint = complaint_key(reporter, *validator, domain, fault);
            if self.complaint_round.read(&complaint)? == round {
                continue;
            }
            self.complaint_round.write(&complaint, round)?;
            let key = complaints_key(*validator, domain, fault);
            let count = self.complaints_at(&key, round)?;
            self.complaints
                .write(&key, round << 16 | u64::from(count.saturating_add(1)))?;
        }
        Ok(())
    }

    /// The number of complaints under `key` that were made in `round`.
    pub(crate) fn complaints_at(&self, key: &B256, round: u64) -> Result<u16> {
        let packed = self.complaints.read(key)?;
        Ok(if packed >> 16 == round {
            packed as u16
        } else {
            0
        })
    }

    /// The miss reasons that come from the reports of the current round, in
    /// the order of `active`.
    pub(crate) fn complaint_reasons(
        &self,
        active: &[Address],
        threshold: usize,
    ) -> Result<Vec<u8>> {
        let round = self.current_round()?;
        let mut reasons = vec![0u8; active.len()];
        for domain in self.domains.read_all()? {
            let not_delivered = Fault::NotDelivered;
            for accused in self.named_by_quorum(active, domain, not_delivered, threshold, round)? {
                reasons[accused] |= miss_reason::NOT_DELIVERED;
            }
            let not_serving = Fault::NotServing;
            for accused in self.named_by_quorum(active, domain, not_serving, threshold, round)? {
                reasons[accused] |= miss_reason::NOT_SERVING;
                for reporter in self.silent_reporters(active, accused, domain, round)? {
                    reasons[reporter] |= miss_reason::NO_COMPLAINT;
                }
            }
        }
        Ok(reasons)
    }

    /// The places in `active` of the validators that at least `threshold`
    /// reports of `round` named for `fault` on `domain`.
    fn named_by_quorum(
        &self,
        active: &[Address],
        domain: u32,
        fault: Fault,
        threshold: usize,
        round: u64,
    ) -> Result<Vec<usize>> {
        let mut named = Vec::new();
        for (place, accused) in active.iter().enumerate() {
            let key = complaints_key(*accused, domain, fault);
            if usize::from(self.complaints_at(&key, round)?) >= threshold {
                named.push(place);
            }
        }
        Ok(named)
    }

    /// The places in `active` of the validators that gave a report for
    /// `domain` in `round` and did not name the validator at the place
    /// `accused` in `notServing`: they did not do the check. The delivery
    /// check has no such rule, because two honest agents can get different
    /// results there.
    fn silent_reporters(
        &self,
        active: &[Address],
        accused: usize,
        domain: u32,
        round: u64,
    ) -> Result<Vec<usize>> {
        let mut silent = Vec::new();
        for (place, reporter) in active.iter().enumerate() {
            if place == accused {
                continue;
            }
            let reported = validator_domain_key(*reporter, domain);
            let complaint = complaint_key(*reporter, active[accused], domain, Fault::NotServing);
            if self.report_round.read(&reported)? == round
                && self.complaint_round.read(&complaint)? != round
            {
                silent.push(place);
            }
        }
        Ok(silent)
    }
}
