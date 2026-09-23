use alloy_primitives::{Address, Bytes};
use alloy_sol_types::{sol, SolCall};
use commonware_codec::DecodeExt;
use commonware_cryptography::bls12381::primitives::group::G2;
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::storage::{SubCallError, SubCallStatus};

use crate::errors::L2RegistryError;
use crate::precompile::IL2Registry;
use crate::schema::{
    L2NetworkRecord, L2NetworkRecordEntryExt, L2RegistryContract, BLS_PUBLIC_KEY_LEN,
};

sol! {
    // Key getter from the settlement contract's IDaInbox interface.
    interface IDaInbox {
        function groupPubKey() external view returns (bytes memory);
    }
}

// Prepaid budget for the external getter, including a proxy's storage reads.
// Unbounded forwarding is unsafe: the native subcall driver does not cap or
// charge the child against the precompile's remaining gas.
const INBOX_KEY_READ_GAS: u64 = 100_000;

impl L2RegistryContract<'_> {
    /// Registers an L2 operator and its 256-byte EIP-2537 root-signing key.
    /// An empty key or 256 zero bytes selects live resolution from its inbox.
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
        let pubkey = decode_optional_public_key(public_key)?;

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

        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            let (pubkey_lo, pubkey_mid, pubkey_hi) = pubkey
                .as_ref()
                .map(L2NetworkRecord::split_public_key)
                .unwrap_or_default();
            self.networks.create(&L2NetworkRecord {
                chain_id,
                l1_address,
                pubkey_lo,
                pubkey_mid,
                pubkey_hi,
            })?;
            self.l1_to_chain.write(&l1_address, chain_id)?;

            self.emit(IL2Registry::L2NetworkRegistered {
                chainId: chain_id,
                l1Address: l1_address,
                publicKey: if pubkey.is_none() {
                    Bytes::from_static(&[0; BLS_PUBLIC_KEY_LEN])
                } else {
                    Bytes::copy_from_slice(public_key)
                },
            })?;
            Ok(())
        })
    }

    /// Rotates the EIP-2537 root-signing key when `caller` is the stored L1 operator,
    /// including when the operator is a contract making an ordinary EVM CALL.
    pub fn update_public_key(
        &mut self,
        caller: Address,
        chain_id: u64,
        public_key: &[u8],
    ) -> Result<()> {
        let record = self.load_network(chain_id)?;
        if caller != record.l1_address {
            return Err(L2RegistryError::NotNetworkOwner { caller, chain_id }.into());
        }
        let pubkey = crate::public_key::decode(public_key)?;

        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            let (pubkey_lo, pubkey_mid, pubkey_hi) = L2NetworkRecord::split_public_key(&pubkey);
            let entry = self.networks.entry(chain_id);
            entry.pubkey_lo().write(pubkey_lo)?;
            entry.pubkey_mid().write(pubkey_mid)?;
            entry.pubkey_hi().write(pubkey_hi)?;
            self.emit(IL2Registry::L2PublicKeyUpdated {
                chainId: chain_id,
                publicKey: Bytes::copy_from_slice(public_key),
            })?;
            Ok(())
        })
    }

    /// Removes a registered network when `caller` is its stored L1 owner.
    pub fn remove_network(&mut self, caller: Address, chain_id: u64) -> Result<()> {
        let record = self.load_network(chain_id)?;
        if caller != record.l1_address {
            return Err(L2RegistryError::NotNetworkOwner { caller, chain_id }.into());
        }

        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            self.networks.delete(chain_id)?;
            self.l1_to_chain.clear(&record.l1_address)?;
            self.emit(IL2Registry::L2NetworkRemoved { chainId: chain_id })?;
            Ok(())
        })
    }

    /// Loads a registration or reverts with `NetworkNotRegistered`.
    pub fn load_network(&self, chain_id: u64) -> Result<L2NetworkRecord> {
        self.networks
            .get(chain_id)?
            .ok_or_else(|| L2RegistryError::NetworkNotRegistered { chain_id }.into())
    }

    /// Resolves the current signing key without caching the inbox's response.
    pub(crate) fn resolve_public_key(&self, record: &L2NetworkRecord) -> Result<G2> {
        let public_key = record.compressed_public_key_bytes();
        if public_key != [0; 96] {
            return G2::decode(public_key.as_slice())
                .map_err(|_| L2RegistryError::InvalidPublicKey.into());
        }

        self.storage.deduct_gas(INBOX_KEY_READ_GAS)?;
        let response = match self.storage.try_staticcall_with_gas(
            record.l1_address,
            IDaInbox::groupPubKeyCall {}.abi_encode().into(),
            INBOX_KEY_READ_GAS,
        ) {
            Ok(response) => response,
            Err(SubCallError::DepthLimitExceeded) => {
                return Err(L2RegistryError::InboxKeyCallFailed.into());
            }
            Err(error) => return Err(error.into()),
        };
        match response.status {
            SubCallStatus::Success => {}
            SubCallStatus::Revert(data) => return Err(PrecompileError::RevertBytes(data)),
            // A hostile or out-of-gas getter must reject this transaction, not
            // abort block execution as a fatal provider failure.
            SubCallStatus::Halt(_) => return Err(L2RegistryError::InboxKeyCallFailed.into()),
        }
        let public_key = IDaInbox::groupPubKeyCall::abi_decode_returns(&response.returndata)
            .map_err(|_| L2RegistryError::InvalidPublicKey)?;
        crate::public_key::decode(&public_key).map_err(|_| L2RegistryError::InvalidPublicKey.into())
    }
}

/// Validates registration keys, with empty/zero EIP-2537 bytes selecting inbox mode.
pub(crate) fn decode_optional_public_key(public_key: &[u8]) -> Result<Option<G2>> {
    if public_key.is_empty()
        || (public_key.len() == BLS_PUBLIC_KEY_LEN && public_key.iter().all(|byte| *byte == 0))
    {
        return Ok(None);
    }
    crate::public_key::decode(public_key).map(Some)
}
