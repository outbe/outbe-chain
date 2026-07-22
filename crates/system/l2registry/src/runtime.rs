use alloy_primitives::{Address, Bytes};
use bytes::Bytes as CodecBytes;
use commonware_codec::DecodeExt;
use commonware_cryptography::bls12381;
use outbe_primitives::error::Result;

use crate::errors::L2RegistryError;
use crate::precompile::IL2Registry;
use crate::schema::{L2NetworkRecord, L2RegistryContract, BLS_PUBLIC_KEY_LEN};

impl L2RegistryContract<'_> {
    /// Registers an L2 network. Permissionless; validation only.
    pub fn register_network(
        &mut self,
        chain_id: u64,
        l1_address: Address,
        public_key: &[u8],
    ) -> Result<()> {
        if chain_id == 0 {
            return Err(L2RegistryError::InvalidChainId.into());
        }
        if l1_address == Address::ZERO {
            return Err(L2RegistryError::InvalidL1Address.into());
        }
        let pubkey: &[u8; BLS_PUBLIC_KEY_LEN] =
            public_key
                .try_into()
                .map_err(|_| L2RegistryError::InvalidPublicKeyLength {
                    length: public_key.len(),
                })?;
        // Group check: the stored key must be a valid MinPk public key so the
        // offer-time verification path can never fail on decode.
        decode_public_key(pubkey)?;

        if self.networks.exists(chain_id)? {
            return Err(L2RegistryError::NetworkAlreadyRegistered { chain_id }.into());
        }
        let existing_chain = self.l1_to_chain.read(&l1_address)?;
        if existing_chain != 0 {
            return Err(L2RegistryError::L1AddressAlreadyRegistered {
                l1_address,
                chain_id: existing_chain,
            }
            .into());
        }

        let (pubkey_lo, pubkey_hi) = L2NetworkRecord::split_public_key(pubkey);
        self.networks.create(&L2NetworkRecord {
            chain_id,
            l1_address,
            pubkey_lo,
            pubkey_hi,
            zk_enabled: false,
        })?;
        self.l1_to_chain.write(&l1_address, chain_id)?;

        self.emit(IL2Registry::L2NetworkRegistered {
            chainId: chain_id,
            l1Address: l1_address,
            publicKey: Bytes::copy_from_slice(public_key),
        })?;
        Ok(())
    }

    /// Enables or disables ZK verification for a registered network.
    pub fn set_zk_enabled(&mut self, chain_id: u64, enabled: bool) -> Result<()> {
        let mut record = self.load_network(chain_id)?;
        record.zk_enabled = enabled;
        self.networks.update(&record)?;
        self.emit(IL2Registry::L2NetworkZkSet {
            chainId: chain_id,
            enabled,
        })?;
        Ok(())
    }

    /// Removes a registered network and its reverse index entry.
    pub fn remove_network(&mut self, chain_id: u64) -> Result<()> {
        let record = self.load_network(chain_id)?;
        self.networks.delete(chain_id)?;
        self.l1_to_chain.clear(&record.l1_address)?;
        self.emit(IL2Registry::L2NetworkRemoved { chainId: chain_id })?;
        Ok(())
    }

    /// Loads a registration or reverts with `NetworkNotRegistered`.
    pub fn load_network(&self, chain_id: u64) -> Result<L2NetworkRecord> {
        self.networks
            .get(chain_id)?
            .ok_or_else(|| L2RegistryError::NetworkNotRegistered { chain_id }.into())
    }

    /// Loads the registration owned by `l1_address`, if any.
    pub(crate) fn network_by_l1_address(
        &self,
        l1_address: Address,
    ) -> Result<Option<L2NetworkRecord>> {
        let chain_id = self.l1_to_chain.read(&l1_address)?;
        if chain_id == 0 {
            return Ok(None);
        }
        self.load_network(chain_id).map(Some)
    }
}

/// Decodes a 48-byte MinPk public key, performing the group check.
pub(crate) fn decode_public_key(pubkey: &[u8; BLS_PUBLIC_KEY_LEN]) -> Result<bls12381::PublicKey> {
    <bls12381::PublicKey as DecodeExt<()>>::decode(CodecBytes::copy_from_slice(pubkey))
        .map_err(|_| L2RegistryError::InvalidPublicKey.into())
}
