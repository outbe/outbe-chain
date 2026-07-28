use std::string::String;

use alloy_primitives::{Address, B256, U256};
use outbe_primitives::addresses::{STABLECOIN_ADDRESS_PREFIX, STABLECOIN_FACTORY_ADDRESS};
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::stablecoin::{
    encode_canonical_stablecoin_create, stablecoin_address, stablecoin_token_id,
};
use outbe_primitives::stablecoin_fork::{
    STABLECOIN_V1_PROTOCOL_VERSION_RAW, STABLECOIN_V1_SCHEMA_VERSION,
};
use outbe_stablecoinpolicy::api::policy_exists;

use crate::api::{FactoryTokenInitialization, TokenIdentity};
use crate::errors::StablecoinStateError;
use crate::schema::{role_key, StablecoinContract, ADMIN_ROLE, OPERATIONAL_ROLES};

const ROOT_SLOT_COUNT: u64 = 19;

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
                    PrecompileError::Fatal(format!(
                    "stablecoin Factory initialization reached incompatible schema {stored_schema}"
                ))
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
            .map_err(|_| PrecompileError::Fatal("stablecoin name storage is not UTF-8".into()))?;
        let symbol = String::from_utf8(self.symbol.read()?)
            .map_err(|_| PrecompileError::Fatal("stablecoin symbol storage is not UTF-8".into()))?;
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
            || initialization.creation_protocol_version
                < u64::from(STABLECOIN_V1_PROTOCOL_VERSION_RAW)
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
