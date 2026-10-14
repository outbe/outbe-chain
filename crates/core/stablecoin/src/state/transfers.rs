use alloy_primitives::{keccak256, Address, B256, U256};
use alloy_sol_types::SolValue;
use k256::ecdsa::{RecoveryId, Signature as EcdsaSignature, VerifyingKey};
use outbe_primitives::error::{PrecompileError, Result};

use super::PERMIT_TYPEHASH;
use crate::abi::IStablecoin;
use crate::api::PermitSignature;
use crate::errors::StablecoinStateError;
use crate::schema::{allowance_key, StablecoinContract};

impl StablecoinContract<'_> {
    pub fn approve(&mut self, owner: Address, spender: Address, value: U256) -> Result<()> {
        self.validated_schema_version()?;
        self.require_nonzero(owner)?;
        self.require_nonzero(spender)?;
        self.validate_allowance_update(owner, spender, value)?;
        let storage = self.storage.clone();
        storage.with_checkpoint(|| self.write_approval(owner, spender, value))
    }

    pub fn permit(
        &mut self,
        owner: Address,
        spender: Address,
        value: U256,
        deadline: U256,
        signature: PermitSignature,
    ) -> Result<()> {
        self.validated_schema_version()?;
        self.require_nonzero(owner)?;
        self.require_nonzero(spender)?;
        if self.storage.timestamp()? > deadline {
            return Err(StablecoinStateError::PermitExpired { deadline }.into());
        }

        let nonce = self.nonces.read(&owner)?;
        let struct_hash =
            keccak256((PERMIT_TYPEHASH, owner, spender, value, nonce, deadline).abi_encode());
        let domain_separator = self.domain_separator()?;
        let mut payload = [0u8; 66];
        payload[..2].copy_from_slice(&[0x19, 0x01]);
        payload[2..34].copy_from_slice(domain_separator.as_slice());
        payload[34..].copy_from_slice(struct_hash.as_slice());
        let digest = keccak256(payload);
        if recover_permit_signer(digest, signature)? != owner {
            return Err(StablecoinStateError::InvalidPermitSignature.into());
        }

        self.validate_allowance_update(owner, spender, value)?;
        let next_nonce = nonce
            .checked_add(U256::from(1))
            .ok_or(StablecoinStateError::PermitNonceOverflow)?;
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            self.nonces.write(&owner, next_nonce)?;
            self.write_approval(owner, spender, value)
        })
    }

    pub fn transfer(&mut self, from: Address, to: Address, value: U256) -> Result<()> {
        self.validated_schema_version()?;
        self.require_not_paused()?;
        self.require_nonzero(from)?;
        self.require_nonzero(to)?;
        self.require_can_send(from)?;
        self.require_can_receive(to)?;
        let from_balance = self.balances.read(&from)?;
        if from_balance < value {
            return Err(StablecoinStateError::InsufficientBalance {
                account: from,
                available: from_balance,
                required: value,
            }
            .into());
        }
        self.require_unfrozen(from, from_balance, value)?;

        let next_to = if from == to {
            from_balance
        } else {
            self.balances
                .read(&to)?
                .checked_add(value)
                .ok_or(StablecoinStateError::RecipientBalanceOverflow)?
        };
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            if from != to {
                self.balances.write(&from, from_balance - value)?;
                self.balances.write(&to, next_to)?;
            }
            self.emit(IStablecoin::Transfer { from, to, value })
        })
    }

    pub fn transfer_from(
        &mut self,
        spender: Address,
        from: Address,
        to: Address,
        value: U256,
    ) -> Result<()> {
        self.validated_schema_version()?;
        self.require_not_paused()?;
        self.require_nonzero(spender)?;
        self.require_nonzero(from)?;
        self.require_nonzero(to)?;
        self.require_can_send(from)?;
        self.require_can_receive(to)?;

        let key = allowance_key(from, spender);
        let allowance = self.allowances.read(&key)?;
        if allowance < value {
            return Err(StablecoinStateError::InsufficientAllowance {
                owner: from,
                spender,
                available: allowance,
                required: value,
            }
            .into());
        }
        let from_balance = self.balances.read(&from)?;
        if from_balance < value {
            return Err(StablecoinStateError::InsufficientBalance {
                account: from,
                available: from_balance,
                required: value,
            }
            .into());
        }
        self.require_unfrozen(from, from_balance, value)?;
        let next_to = if from == to {
            from_balance
        } else {
            self.balances
                .read(&to)?
                .checked_add(value)
                .ok_or(StablecoinStateError::RecipientBalanceOverflow)?
        };

        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            if allowance != U256::MAX {
                self.allowances.write(&key, allowance - value)?;
            }
            if from != to {
                self.balances.write(&from, from_balance - value)?;
                self.balances.write(&to, next_to)?;
            }
            self.emit(IStablecoin::Transfer { from, to, value })
        })
    }

    pub fn transfer_with_memo(
        &mut self,
        from: Address,
        to: Address,
        amount: U256,
        memo: B256,
    ) -> Result<()> {
        self.with_memo(from, to, amount, memo, |token| {
            token.transfer(from, to, amount)
        })
    }

    pub fn transfer_from_with_memo(
        &mut self,
        spender: Address,
        from: Address,
        to: Address,
        amount: U256,
        memo: B256,
    ) -> Result<()> {
        self.with_memo(from, to, amount, memo, |token| {
            token.transfer_from(spender, from, to, amount)
        })
    }

    fn validate_allowance_update(
        &self,
        owner: Address,
        spender: Address,
        value: U256,
    ) -> Result<()> {
        let current = self.allowances.read(&allowance_key(owner, spender))?;
        if !self.frozen.read(&owner)?.is_zero() && value > current {
            return Err(StablecoinStateError::FrozenAllowanceIncrease {
                owner,
                current_allowance: current,
                requested_allowance: value,
            }
            .into());
        }
        Ok(())
    }

    fn write_approval(&mut self, owner: Address, spender: Address, value: U256) -> Result<()> {
        self.allowances
            .write(&allowance_key(owner, spender), value)?;
        self.emit(IStablecoin::Approval {
            owner,
            spender,
            value,
        })
    }

    pub(super) fn with_memo(
        &mut self,
        from: Address,
        to: Address,
        amount: U256,
        memo: B256,
        operation: impl FnOnce(&mut Self) -> Result<()>,
    ) -> Result<()> {
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            operation(self)?;
            self.emit(IStablecoin::TransferWithMemo {
                from,
                to,
                amount,
                memo,
            })
        })
    }
}

fn recover_permit_signer(digest: B256, signature: PermitSignature) -> Result<Address> {
    let PermitSignature { v, r, s } = signature;
    let recovery_id = match v {
        27 | 28 => RecoveryId::from_byte(v - 27),
        _ => None,
    }
    .ok_or_else(|| PrecompileError::from(StablecoinStateError::InvalidPermitSignature))?;

    let mut signature_bytes = [0u8; 64];
    signature_bytes[..32].copy_from_slice(r.as_slice());
    signature_bytes[32..].copy_from_slice(s.as_slice());
    let signature = EcdsaSignature::from_slice(&signature_bytes)
        .map_err(|_| PrecompileError::from(StablecoinStateError::InvalidPermitSignature))?;
    if signature.normalize_s().is_some() {
        return Err(StablecoinStateError::InvalidPermitSignature.into());
    }
    let verifying_key =
        VerifyingKey::recover_from_prehash(digest.as_slice(), &signature, recovery_id)
            .map_err(|_| PrecompileError::from(StablecoinStateError::InvalidPermitSignature))?;
    let encoded = verifying_key.to_encoded_point(false);
    let public_key = encoded.as_bytes();
    let hash = keccak256(&public_key[1..]);
    Ok(Address::from_slice(&hash[12..]))
}
