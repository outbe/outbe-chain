//! Validator sync: the active Outbe validator set goes into every ISM.
//!
//! `sync` and begin-block liveness call `set_validators_and_threshold`. Each
//! ISM call goes through the [`Owner`].

use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::SolCall;
use outbe_primitives::error::Result;

use crate::abi::ICrosschainGov;
use crate::errors::HyperlaneControllerError;
use crate::runtime::{consensus_threshold, validate_validators};
use crate::schema::HyperlaneControllerContract;
use crate::sol_ext::IStorageMultisigIsm;
use crate::{Owner, RemoteCall};

impl HyperlaneControllerContract<'_> {
    /// Mirrors the active Outbe validator set into every ISM (validators =
    /// the Hyperlane signer of every active validator, threshold =
    /// [`consensus_threshold`]). Returns `false` when the local ISM already
    /// matches, so a keeper can call it every epoch.
    pub fn sync(&mut self, owner: &mut impl Owner) -> Result<bool> {
        let active = self.active_signers()?;
        let threshold = consensus_threshold(active.len())?;
        let (current, current_threshold) = self.current_validators()?;
        if current_threshold == threshold && same_set(&current, &active) {
            return Ok(false);
        }
        self.set_validators_and_threshold(&active, threshold, owner)?;
        Ok(true)
    }

    /// Full rotation: `setValidatorsAndThreshold` on every remote ISM through
    /// the Interchain Account, then on the local ISM. All calls run under one
    /// checkpoint. Any failure leaves every ISM untouched.
    pub fn set_validators_and_threshold(
        &mut self,
        validators: &[Address],
        threshold: u8,
        owner: &mut impl Owner,
    ) -> Result<()> {
        validate_validators(validators, threshold)?;
        let local = self.local_domain()?;
        let local_ism = self.local_ism()?;
        let data: Bytes = IStorageMultisigIsm::setValidatorsAndThresholdCall {
            _validators: validators.to_vec(),
            _threshold: threshold,
        }
        .abi_encode()
        .into();

        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            for domain in self.domains.read_all()? {
                if domain == local {
                    continue;
                }
                let ism = self.ism_by_domain.read(&domain)?;
                owner.call_remote(domain, &[RemoteCall::new(ism, data.clone())])?;
            }
            owner.call_local(local_ism, U256::ZERO, data)?;
            self.emit(ICrosschainGov::ValidatorsAndThresholdApplied {
                threshold,
                validatorCount: U256::from(validators.len()),
            })
        })
    }

    /// Current set as stored by the local ISM (the source of truth).
    pub(crate) fn current_validators(&self) -> Result<(Vec<Address>, u8)> {
        let local_ism = self.local_ism()?;
        let ret = self.storage.staticcall(
            local_ism,
            IStorageMultisigIsm::validatorsAndThresholdCall {
                _message: Bytes::new(),
            }
            .abi_encode()
            .into(),
        )?;
        let decoded = IStorageMultisigIsm::validatorsAndThresholdCall::abi_decode_returns(&ret)
            .map_err(|_| HyperlaneControllerError::UndecodableReturn("validatorsAndThreshold"))?;
        Ok((decoded._0, decoded._1))
    }
}

fn same_set(left: &[Address], right: &[Address]) -> bool {
    let mut left = left.to_vec();
    let mut right = right.to_vec();
    left.sort_unstable();
    right.sort_unstable();
    left == right
}
