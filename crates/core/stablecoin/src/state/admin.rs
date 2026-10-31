use alloy_primitives::{Address, B256, U256};
use outbe_primitives::error::Result;
use outbe_stablecoinpolicy::api::policy_exists;

use crate::abi::IStablecoin;
use crate::errors::StablecoinStateError;
use crate::schema::{
    role_key, StablecoinContract, ADMIN_ROLE, CAP_MANAGER_ROLE, GUARDIAN_ROLE, OPERATIONAL_ROLES,
};

impl StablecoinContract<'_> {
    pub fn grant_role(&mut self, actor: Address, role: B256, account: Address) -> Result<()> {
        self.require_role(ADMIN_ROLE, actor)?;
        self.require_operational_role(role)?;
        self.require_nonzero(account)?;
        let key = role_key(role, account);
        if self.roles.read(&key)? {
            return Ok(());
        }
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            self.roles.write(&key, true)?;
            self.emit(IStablecoin::RoleGranted {
                role,
                account,
                actor,
            })
        })
    }

    pub fn revoke_role(&mut self, actor: Address, role: B256, account: Address) -> Result<()> {
        self.require_role(ADMIN_ROLE, actor)?;
        self.require_operational_role(role)?;
        self.require_nonzero(account)?;
        let key = role_key(role, account);
        if !self.roles.read(&key)? {
            return Ok(());
        }
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            self.roles.write(&key, false)?;
            self.emit(IStablecoin::RoleRevoked {
                role,
                account,
                actor,
            })
        })
    }

    pub fn begin_admin_transfer(&mut self, actor: Address, candidate: Address) -> Result<()> {
        self.require_role(ADMIN_ROLE, actor)?;
        if candidate == Address::ZERO || candidate == actor {
            return Err(StablecoinStateError::InvalidPendingAdmin { candidate }.into());
        }
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            self.pending_admin.write(candidate)?;
            self.emit(IStablecoin::AdminTransferStarted {
                currentAdmin: actor,
                pendingAdmin: candidate,
            })
        })
    }

    pub fn cancel_admin_transfer(&mut self, actor: Address) -> Result<()> {
        self.require_role(ADMIN_ROLE, actor)?;
        let pending = self.pending_admin.read()?;
        if pending == Address::ZERO {
            return Err(StablecoinStateError::InvalidPendingAdmin {
                candidate: Address::ZERO,
            }
            .into());
        }
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            self.pending_admin.write(Address::ZERO)?;
            self.emit(IStablecoin::AdminTransferCanceled {
                admin: actor,
                canceledPendingAdmin: pending,
            })
        })
    }

    pub fn accept_admin_transfer(&mut self, actor: Address) -> Result<()> {
        self.validated_schema_version()?;
        let pending = self.pending_admin.read()?;
        if actor == Address::ZERO || actor != pending {
            return Err(StablecoinStateError::NotPendingAdmin { caller: actor }.into());
        }
        let previous = self.admin.read()?;
        if previous == Address::ZERO
            || !self.roles.read(&role_key(ADMIN_ROLE, previous))?
            || self.roles.read(&role_key(ADMIN_ROLE, actor))?
        {
            return Err(StablecoinStateError::InconsistentAdminTransferRoleState.into());
        }
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            self.roles.write(&role_key(ADMIN_ROLE, previous), false)?;
            self.roles.write(&role_key(ADMIN_ROLE, actor), true)?;
            self.admin.write(actor)?;
            self.pending_admin.write(Address::ZERO)?;
            self.emit(IStablecoin::AdminTransferred {
                previousAdmin: previous,
                newAdmin: actor,
            })
        })
    }

    pub fn pause(&mut self, actor: Address) -> Result<()> {
        self.require_role(GUARDIAN_ROLE, actor)?;
        if self.paused.read()? {
            return Err(StablecoinStateError::TokenPaused.into());
        }
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            self.paused.write(true)?;
            self.emit(IStablecoin::Paused { actor })
        })
    }

    pub fn unpause(&mut self, actor: Address) -> Result<()> {
        self.require_role(ADMIN_ROLE, actor)?;
        if !self.paused.read()? {
            return Err(StablecoinStateError::TokenNotPaused.into());
        }
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            self.paused.write(false)?;
            self.emit(IStablecoin::Unpaused { actor })
        })
    }

    pub fn set_supply_cap(&mut self, actor: Address, new_cap: U256) -> Result<()> {
        self.require_role(CAP_MANAGER_ROLE, actor)?;
        let supply = self.total_supply.read()?;
        if new_cap < supply {
            return Err(StablecoinStateError::SupplyCapBelowSupply {
                requested_cap: new_cap,
                total_supply: supply,
            }
            .into());
        }
        let previous = self.supply_cap.read()?;
        if previous == new_cap {
            return Ok(());
        }
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            self.supply_cap.write(new_cap)?;
            self.emit(IStablecoin::SupplyCapChanged {
                previousCap: previous,
                newCap: new_cap,
                actor,
            })
        })
    }

    pub fn set_policy_id(&mut self, actor: Address, new_policy_id: U256) -> Result<()> {
        self.require_role(crate::schema::COMPLIANCE_ROLE, actor)?;
        if !policy_exists(self.storage.clone(), new_policy_id)? {
            return Err(StablecoinStateError::UnknownPolicy {
                policy_id: new_policy_id,
            }
            .into());
        }
        let previous = self.policy_id.read()?;
        if previous == new_policy_id {
            return Ok(());
        }
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            self.policy_id.write(new_policy_id)?;
            self.emit(IStablecoin::PolicyChanged {
                previousPolicyId: previous,
                newPolicyId: new_policy_id,
                actor,
            })
        })
    }

    fn require_operational_role(&self, role: B256) -> Result<()> {
        if !OPERATIONAL_ROLES.contains(&role) {
            return Err(StablecoinStateError::UnsupportedRole { role }.into());
        }
        Ok(())
    }
}
