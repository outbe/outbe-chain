use std::collections::HashSet;

use alloy_primitives::{Address, U256};
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::stablecoin_fork::{
    STABLECOIN_LIST_PAGE_CAP, STABLECOIN_POLICY_MEMBER_BATCH_CAP,
};

use crate::abi::IStablecoinPolicyRegistry;
use crate::errors::PolicyRegistryError;
use crate::schema::{PolicyType, StablecoinPolicyRegistryContract};

impl StablecoinPolicyRegistryContract<'_> {
    pub fn is_member(&self, policy_id: U256, account: Address) -> Result<bool> {
        let Some(descriptor) = self.optional_descriptor(policy_id)? else {
            return Ok(false);
        };
        let policy_type = match PolicyType::try_from(descriptor.policy_type) {
            Ok(policy_type) => policy_type,
            Err(_) => return Ok(false),
        };
        if !policy_type.is_simple() {
            return Ok(false);
        }
        self.member_set(policy_id).contains(&account)
    }

    pub fn policy_member_count(&self, policy_id: U256) -> Result<U256> {
        let policy_type = self.policy_type(policy_id)?;
        self.require_enumerable(policy_id, policy_type)?;
        self.member_set(policy_id).len().map(U256::from)
    }

    pub fn list_policy_members(
        &self,
        policy_id: U256,
        offset: U256,
        limit: U256,
    ) -> Result<Vec<Address>> {
        if limit == U256::ZERO || limit > U256::from(STABLECOIN_LIST_PAGE_CAP) {
            return Err(PolicyRegistryError::InvalidListLimit {
                limit,
                maximum: STABLECOIN_LIST_PAGE_CAP,
            }
            .into());
        }
        let policy_type = self.policy_type(policy_id)?;
        self.require_enumerable(policy_id, policy_type)?;

        let members = self.member_set(policy_id);
        let count = members.len()?;
        if offset >= U256::from(count) {
            return Ok(Vec::new());
        }

        let start = offset.to::<u32>();
        let requested = limit.to::<u32>();
        let end = start.saturating_add(requested).min(count);
        let mut page = Vec::with_capacity((end - start) as usize);
        for index in start..end {
            let member = members.at(index)?.ok_or_else(|| {
                PrecompileError::Fatal("policy member set length/data mismatch".into())
            })?;
            page.push(member);
        }
        Ok(page)
    }

    pub fn add_members(
        &mut self,
        policy_id: U256,
        caller: Address,
        accounts: &[Address],
    ) -> Result<()> {
        self.mutate_members(policy_id, caller, accounts, true)
    }

    pub fn remove_members(
        &mut self,
        policy_id: U256,
        caller: Address,
        accounts: &[Address],
    ) -> Result<()> {
        self.mutate_members(policy_id, caller, accounts, false)
    }

    fn validate_member_batch(&self, accounts: &[Address]) -> Result<()> {
        if accounts.is_empty() {
            return Err(PolicyRegistryError::EmptyMemberBatch.into());
        }
        if accounts.len() > STABLECOIN_POLICY_MEMBER_BATCH_CAP {
            return Err(PolicyRegistryError::MemberBatchTooLarge {
                length: accounts.len(),
                maximum: STABLECOIN_POLICY_MEMBER_BATCH_CAP,
            }
            .into());
        }
        let mut unique = HashSet::with_capacity(accounts.len());
        for account in accounts {
            if *account == Address::ZERO {
                return Err(PolicyRegistryError::InvalidMember { account: *account }.into());
            }
            if !unique.insert(*account) {
                return Err(PolicyRegistryError::DuplicateMember { account: *account }.into());
            }
        }
        Ok(())
    }

    fn mutate_members(
        &mut self,
        policy_id: U256,
        caller: Address,
        accounts: &[Address],
        add: bool,
    ) -> Result<()> {
        let descriptor = self.mutable_descriptor_for_admin(policy_id, caller)?;
        let policy_type = self.descriptor_type(&descriptor)?;
        if !policy_type.is_simple() {
            return Err(PolicyRegistryError::DirectionalMembershipUnsupported { policy_id }.into());
        }
        self.validate_member_batch(accounts)?;

        let members = self.member_set(policy_id);
        for account in accounts {
            if members.contains(account)? == add {
                return Err(PolicyRegistryError::MembershipUnchanged {
                    policy_id,
                    account: *account,
                    member: add,
                }
                .into());
            }
        }

        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            for account in accounts {
                let changed = if add {
                    members.insert(*account)?
                } else {
                    members.remove(account)?
                };
                if !changed {
                    return Err(PrecompileError::Fatal(
                        "validated policy membership changed during mutation".into(),
                    ));
                }
                if add {
                    self.emit(IStablecoinPolicyRegistry::PolicyMemberAdded {
                        policyId: policy_id,
                        account: *account,
                        actor: caller,
                    })?;
                } else {
                    self.emit(IStablecoinPolicyRegistry::PolicyMemberRemoved {
                        policyId: policy_id,
                        account: *account,
                        actor: caller,
                    })?;
                }
            }
            Ok(())
        })
    }
}
