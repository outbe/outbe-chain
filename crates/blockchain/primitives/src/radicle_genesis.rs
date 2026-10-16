//! Cycle-free OGD1 governments descriptor, known before the genesis block hash.

use alloy_primitives::{FixedBytes, B256};
use commonware_cryptography::{Hasher as _, Sha256};

use crate::error::{PrecompileError, Result};

pub const GENESIS_GOVERNMENTS_FIELD: &str = "radicleGovernments";
pub const GOVERNMENTS_DESCRIPTOR_HASH_SLOT: u64 = 10;
pub const GOVERNMENTS_DESCRIPTOR_BYTES_SLOT: u64 = 11;
pub const MAX_GOVERNMENTS_DESCRIPTION_BYTES: usize = 256;
pub const MIN_GOVERNMENTS_DESCRIPTOR_BYTES: usize = 97;
pub const MAX_GOVERNMENTS_DESCRIPTOR_BYTES: usize =
    MIN_GOVERNMENTS_DESCRIPTOR_BYTES + MAX_GOVERNMENTS_DESCRIPTION_BYTES;

pub fn validate_publication_window(epoch_length: u64, prepare_window: u64) -> Result<()> {
    if epoch_length.saturating_sub(prepare_window.min(epoch_length)) < 64 {
        return Err(PrecompileError::revert(
            "governments requires at least 64 blocks before the next DKG freeze",
        ));
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GovernmentsDescriptor {
    pub chain_id: u64,
    pub genesis_timestamp: u64,
    pub description: String,
    pub initial_tree_oid: FixedBytes<20>,
    pub initial_content_manifest_sha256: B256,
}

impl GovernmentsDescriptor {
    pub fn validate(&self, chain_id: u64, genesis_timestamp: u64) -> Result<()> {
        if self.chain_id != chain_id
            || self.genesis_timestamp != genesis_timestamp
            || self.description.len() > MAX_GOVERNMENTS_DESCRIPTION_BYTES
            || self.description.contains(['\r', '\0'])
            || self.initial_tree_oid.is_zero()
            || self.initial_content_manifest_sha256.is_zero()
        {
            return Err(PrecompileError::revert(
                "invalid governments genesis descriptor",
            ));
        }
        Ok(())
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        self.validate(self.chain_id, self.genesis_timestamp)?;
        Ok([
            b"OGD1".as_slice(),
            &1_u16.to_be_bytes(),
            &self.chain_id.to_be_bytes(),
            &self.genesis_timestamp.to_be_bytes(),
            &11_u16.to_be_bytes(),
            b"governments",
            &4_u16.to_be_bytes(),
            b"main",
            &(self.description.len() as u16).to_be_bytes(),
            self.description.as_bytes(),
            self.initial_tree_oid.as_slice(),
            self.initial_content_manifest_sha256.as_slice(),
            &[1, 1],
        ]
        .concat())
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_GOVERNMENTS_DESCRIPTOR_BYTES {
            return Err(PrecompileError::revert("oversized governments descriptor"));
        }
        let mut input = bytes;
        if take::<4>(&mut input)? != *b"OGD1" || take::<2>(&mut input)? != 1_u16.to_be_bytes() {
            return Err(PrecompileError::revert(
                "unsupported governments descriptor",
            ));
        }
        let chain_id = u64::from_be_bytes(take(&mut input)?);
        let genesis_timestamp = u64::from_be_bytes(take(&mut input)?);
        if take::<2>(&mut input)? != 11_u16.to_be_bytes()
            || take::<11>(&mut input)? != *b"governments"
            || take::<2>(&mut input)? != 4_u16.to_be_bytes()
            || take::<4>(&mut input)? != *b"main"
        {
            return Err(PrecompileError::revert(
                "governments descriptor requires governments/main",
            ));
        }
        let length = usize::from(u16::from_be_bytes(take(&mut input)?));
        if length > MAX_GOVERNMENTS_DESCRIPTION_BYTES {
            return Err(PrecompileError::revert("oversized governments description"));
        }
        let (description, tail) = input
            .split_at_checked(length)
            .ok_or_else(|| PrecompileError::revert("truncated governments description"))?;
        let description = String::from_utf8(description.to_vec())
            .map_err(|_| PrecompileError::revert("governments description is not UTF-8"))?;
        input = tail;
        let descriptor = Self {
            chain_id,
            genesis_timestamp,
            description,
            initial_tree_oid: take::<20>(&mut input)?.into(),
            initial_content_manifest_sha256: take::<32>(&mut input)?.into(),
        };
        if take::<2>(&mut input)? != [1, 1] || !input.is_empty() {
            return Err(PrecompileError::revert(
                "invalid governments policy/key source or trailing bytes",
            ));
        }
        descriptor.validate(chain_id, genesis_timestamp)?;
        Ok(descriptor)
    }

    pub fn hash(&self) -> Result<B256> {
        Ok(Sha256::hash(&[&self.to_bytes()?]).0.into())
    }

    /// The optional genesis field contains the exact pre-genesis OGD1 artifact as hex.
    pub fn from_genesis_field(
        value: Option<&serde_json::Value>,
        chain_id: u64,
        genesis_timestamp: u64,
        seeded_hash: B256,
    ) -> Result<Option<Self>> {
        let Some(value) = value else {
            return if seeded_hash.is_zero() {
                Ok(None)
            } else {
                Err(PrecompileError::revert(
                    "seeded governments descriptor is missing from genesis config",
                ))
            };
        };
        let text = value
            .as_str()
            .and_then(|text| text.strip_prefix("0x"))
            .filter(|text| text.len() <= MAX_GOVERNMENTS_DESCRIPTOR_BYTES * 2)
            .ok_or_else(|| {
                PrecompileError::revert("radicleGovernments must be a bounded hex OGD1 descriptor")
            })?;
        let bytes = hex::decode(text)
            .map_err(|_| PrecompileError::revert("invalid governments descriptor hex"))?;
        let descriptor = Self::from_bytes(&bytes)?;
        descriptor.validate(chain_id, genesis_timestamp)?;
        if descriptor.hash()? != seeded_hash {
            return Err(PrecompileError::revert(
                "governments descriptor differs from seeded genesis state",
            ));
        }
        Ok(Some(descriptor))
    }
}

fn take<const N: usize>(input: &mut &[u8]) -> Result<[u8; N]> {
    let (head, tail) = input
        .split_at_checked(N)
        .ok_or_else(|| PrecompileError::revert("truncated governments descriptor"))?;
    *input = tail;
    head.try_into()
        .map_err(|_| PrecompileError::revert("invalid governments descriptor field"))
}

#[cfg(test)]
mod tests;
