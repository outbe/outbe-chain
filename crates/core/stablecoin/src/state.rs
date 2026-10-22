use std::string::String;

use alloy_primitives::{b256, keccak256, Address, B256, U256};
use alloy_sol_types::SolValue;
use outbe_primitives::addresses::{STABLECOIN_ADDRESS_PREFIX, STABLECOIN_FACTORY_ADDRESS};
use outbe_primitives::error::Result;
use outbe_primitives::stablecoin::{
    encode_canonical_stablecoin_create, stablecoin_address, stablecoin_token_id,
};
use outbe_primitives::stablecoin_fork::STABLECOIN_V1_SCHEMA_VERSION;
use outbe_stablecoinpolicy::api::{
    can_receive as policy_can_receive, can_send as policy_can_send, policy_exists,
};

use crate::api::{FactoryTokenInitialization, TokenIdentity};
use crate::errors::StablecoinStateError;
use crate::schema::{allowance_key, role_key, StablecoinContract, ADMIN_ROLE, OPERATIONAL_ROLES};

const ROOT_SLOT_COUNT: u64 = 19;
const EIP712_DOMAIN_TYPEHASH: B256 =
    b256!("8b73c3c69bb8fe3d512ecc4cf759cc79239f7b179b0ffacaa9a75d522b39400f");
const EIP712_VERSION_HASH: B256 =
    b256!("c89efdaa54c0f20c7adf612882df0950f5a951637e0307cdcb4c672f298b8bc6");
const PERMIT_TYPEHASH: B256 =
    b256!("6e71edae12b1b97f4d1f60370fef10105fa2faae0126114a169c64845d6126c9");

mod admin;
mod freeze;
mod supply;
mod transfers;

impl StablecoinContract<'_> {
    pub(crate) fn initialize_from_factory(
        &self,
        initialization: &FactoryTokenInitialization,
    ) -> Result<()> {
        let stored_schema = self.schema_version.read()?;
        if stored_schema != 0 {
            return Err(
                if stored_schema == u64::from(STABLECOIN_V1_SCHEMA_VERSION) {
                    StablecoinStateError::AlreadyInitialized.into()
                } else {
                    StablecoinStateError::IncompatibleInitializationSchema { stored_schema }.into()
                },
            );
        }
        self.validate_pristine_root()?;
        self.validate_initialization(initialization)?;

        let payload = &initialization.payload;
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            self.creation_protocol_version
                .write(initialization.creation_protocol_version)?;
            self.token_id.write(initialization.token_id)?;
            self.name.write_string(&payload.name)?;
            self.symbol.write_string(&payload.ticker)?;
            self.currency.write(payload.iso4217)?;
            self.decimals.write(payload.decimals)?;
            self.issuer.write(payload.issuer)?;
            self.supply_cap.write(payload.supply_cap)?;
            self.policy_id.write(payload.policy_id)?;
            self.admin.write(payload.issuer)?;
            self.roles
                .write(&role_key(ADMIN_ROLE, payload.issuer), true)?;
            for role in OPERATIONAL_ROLES {
                self.roles.write(&role_key(role, payload.issuer), true)?;
            }
            // The schema marker is the final write: schema 1 always denotes a
            // completely initialized zero-supply ledger.
            self.schema_version
                .write(u64::from(STABLECOIN_V1_SCHEMA_VERSION))
        })
    }

    pub(crate) fn validated_schema_version(&self) -> Result<u64> {
        let stored = self.schema_version.read()?;
        match stored {
            0 => Err(StablecoinStateError::Uninitialized.into()),
            value if value == u64::from(STABLECOIN_V1_SCHEMA_VERSION) => Ok(value),
            value => Err(StablecoinStateError::MigrationRequired {
                stored_schema_version: value,
                active_schema_version: u64::from(STABLECOIN_V1_SCHEMA_VERSION),
            }
            .into()),
        }
    }

    pub(crate) fn identity(&self) -> Result<TokenIdentity> {
        self.validated_schema_version()?;
        let name = String::from_utf8(self.name.read()?)
            .map_err(|_| StablecoinStateError::InvalidNameUtf8)?;
        let symbol = String::from_utf8(self.symbol.read()?)
            .map_err(|_| StablecoinStateError::InvalidSymbolUtf8)?;
        Ok(TokenIdentity {
            token_id: self.token_id.read()?,
            name,
            symbol,
            currency: self.currency.read()?,
            decimals: self.decimals.read()?,
            issuer: self.issuer.read()?,
            creation_protocol_version: self.creation_protocol_version.read()?,
        })
    }

    pub fn name_value(&self) -> Result<String> {
        self.validated_schema_version()?;
        String::from_utf8(self.name.read()?)
            .map_err(|_| StablecoinStateError::InvalidNameUtf8.into())
    }

    pub fn symbol_value(&self) -> Result<String> {
        self.validated_schema_version()?;
        String::from_utf8(self.symbol.read()?)
            .map_err(|_| StablecoinStateError::InvalidSymbolUtf8.into())
    }

    pub fn decimals_value(&self) -> Result<u8> {
        self.validated_schema_version()?;
        self.decimals.read()
    }

    pub fn total_supply(&self) -> Result<U256> {
        self.validated_schema_version()?;
        self.total_supply.read()
    }

    pub fn balance_of(&self, account: Address) -> Result<U256> {
        self.validated_schema_version()?;
        self.balances.read(&account)
    }

    pub fn allowance_of(&self, owner: Address, spender: Address) -> Result<U256> {
        self.validated_schema_version()?;
        self.allowances.read(&allowance_key(owner, spender))
    }

    pub fn domain_separator(&self) -> Result<B256> {
        self.validated_schema_version()?;
        let name_hash = keccak256(self.name.read()?);
        let chain_id = U256::from(self.storage.chain_id()?);
        Ok(keccak256(
            (
                EIP712_DOMAIN_TYPEHASH,
                name_hash,
                EIP712_VERSION_HASH,
                chain_id,
                self.address,
            )
                .abi_encode(),
        ))
    }

    pub fn has_role(&self, role: B256, account: Address) -> Result<bool> {
        self.validated_schema_version()?;
        if role != ADMIN_ROLE && !OPERATIONAL_ROLES.contains(&role) {
            return Ok(false);
        }
        let member = self.roles.read(&role_key(role, account))?;
        if role == ADMIN_ROLE {
            let admin = self.admin.read()?;
            if member != (account == admin) {
                return Err(StablecoinStateError::AdminRoleMismatch.into());
            }
        }
        Ok(member)
    }

    pub fn can_send(&self, account: Address) -> Result<bool> {
        self.validated_schema_version()?;
        if self.paused.read()? || account == Address::ZERO {
            return Ok(false);
        }
        policy_can_send(self.storage.clone(), self.policy_id.read()?, account)
    }

    pub fn can_receive(&self, account: Address) -> Result<bool> {
        self.validated_schema_version()?;
        if self.paused.read()? || account == Address::ZERO {
            return Ok(false);
        }
        policy_can_receive(self.storage.clone(), self.policy_id.read()?, account)
    }

    pub fn can_transfer(&self, from: Address, to: Address, value: U256) -> Result<bool> {
        self.validated_schema_version()?;
        if self.paused.read()? || from == Address::ZERO || to == Address::ZERO {
            return Ok(false);
        }
        let policy_id = self.policy_id.read()?;
        if !policy_can_send(self.storage.clone(), policy_id, from)?
            || !policy_can_receive(self.storage.clone(), policy_id, to)?
        {
            return Ok(false);
        }
        let balance = self.balances.read(&from)?;
        let frozen = self.frozen.read(&from)?;
        Ok(value <= Self::unfrozen(balance, frozen))
    }

    fn require_nonzero(&self, account: Address) -> Result<()> {
        if account == Address::ZERO {
            return Err(StablecoinStateError::InvalidAddress { account }.into());
        }
        Ok(())
    }

    fn require_nonzero_amount(&self, amount: U256) -> Result<()> {
        if amount.is_zero() {
            return Err(StablecoinStateError::InvalidAmount.into());
        }
        Ok(())
    }

    fn require_not_paused(&self) -> Result<()> {
        if self.paused.read()? {
            return Err(StablecoinStateError::TokenPaused.into());
        }
        Ok(())
    }

    fn require_role(&self, role: B256, caller: Address) -> Result<()> {
        if !self.has_role(role, caller)? {
            return Err(StablecoinStateError::Unauthorized { role, caller }.into());
        }
        Ok(())
    }

    fn require_can_send(&self, account: Address) -> Result<()> {
        let policy_id = self.policy_id.read()?;
        if !policy_can_send(self.storage.clone(), policy_id, account)? {
            return Err(StablecoinStateError::ERC7943CannotSend { account }.into());
        }
        Ok(())
    }

    fn require_can_receive(&self, account: Address) -> Result<()> {
        let policy_id = self.policy_id.read()?;
        if !policy_can_receive(self.storage.clone(), policy_id, account)? {
            return Err(StablecoinStateError::ERC7943CannotReceive { account }.into());
        }
        Ok(())
    }

    fn unfrozen(balance: U256, frozen: U256) -> U256 {
        balance.saturating_sub(frozen)
    }

    fn validate_pristine_root(&self) -> Result<()> {
        for slot in 1..ROOT_SLOT_COUNT {
            if !self
                .storage
                .sload(self.address, U256::from(slot))?
                .is_zero()
            {
                return Err(StablecoinStateError::NonPristineRoot { slot }.into());
            }
        }
        Ok(())
    }

    fn validate_initialization(&self, initialization: &FactoryTokenInitialization) -> Result<()> {
        if initialization.token_address != self.address
            || initialization.token_address == Address::ZERO
            || initialization.token_id == B256::ZERO
        {
            return Err(StablecoinStateError::InvalidInitializationIdentity.into());
        }

        // This validates all immutable metadata fields even when a trusted Rust
        // caller constructed StablecoinCreatePayload directly.
        encode_canonical_stablecoin_create(&initialization.payload)
            .map_err(|_| StablecoinStateError::InvalidInitializationIdentity)?;

        let chain_id = self.storage.chain_id()?;
        let expected_id = stablecoin_token_id(
            chain_id,
            STABLECOIN_FACTORY_ADDRESS,
            initialization.payload.issuer,
            &initialization.payload.ticker,
        )
        .map_err(|_| StablecoinStateError::InvalidInitializationIdentity)?;
        let expected_address = stablecoin_address(expected_id, STABLECOIN_ADDRESS_PREFIX);
        if expected_id != initialization.token_id || expected_address != self.address {
            return Err(StablecoinStateError::InvalidInitializationIdentity.into());
        }
        if !policy_exists(self.storage.clone(), initialization.payload.policy_id)? {
            return Err(StablecoinStateError::UnknownInitializationPolicy {
                policy_id: initialization.payload.policy_id,
            }
            .into());
        }
        Ok(())
    }
}
