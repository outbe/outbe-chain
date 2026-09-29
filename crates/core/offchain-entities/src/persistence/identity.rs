use super::{Decoder, PersistenceError, LOCAL_STORAGE_SCHEMA_VERSION};
use alloy_primitives::B256;

/// Persistent environment identity. A mismatch requires an explicit rebuild or
/// migration; it is never silently reinterpreted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnvironmentIdentity {
    pub local_storage_schema_version: u32,
    pub chain_id: u64,
    pub genesis_hash: B256,
    pub commitment_scheme_version: u32,
    pub topology: Vec<u8>,
    pub tree_format: String,
    pub vendor_revision: String,
}

impl EnvironmentIdentity {
    pub fn encode(&self) -> Result<Vec<u8>, PersistenceError> {
        let tree = self.tree_format.as_bytes();
        let vendor = self.vendor_revision.as_bytes();
        let tree_len = u16::try_from(tree.len()).map_err(|_| PersistenceError::LengthOverflow)?;
        let vendor_len =
            u16::try_from(vendor.len()).map_err(|_| PersistenceError::LengthOverflow)?;
        let topology_len =
            u16::try_from(self.topology.len()).map_err(|_| PersistenceError::LengthOverflow)?;
        crate::CeTopologyV1::decode(&self.topology)
            .map_err(|_| PersistenceError::InvalidTopologyIdentity)?;
        let mut bytes = Vec::with_capacity(58 + self.topology.len() + tree.len() + vendor.len());
        bytes.extend_from_slice(&self.local_storage_schema_version.to_be_bytes());
        bytes.extend_from_slice(&self.chain_id.to_be_bytes());
        bytes.extend_from_slice(self.genesis_hash.as_slice());
        bytes.extend_from_slice(&self.commitment_scheme_version.to_be_bytes());
        bytes.extend_from_slice(&topology_len.to_be_bytes());
        bytes.extend_from_slice(&self.topology);
        bytes.extend_from_slice(&tree_len.to_be_bytes());
        bytes.extend_from_slice(tree);
        bytes.extend_from_slice(&vendor_len.to_be_bytes());
        bytes.extend_from_slice(vendor);
        Ok(bytes)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, PersistenceError> {
        let mut decoder = Decoder::new(bytes, "environment identity");
        let local_storage_schema_version = decoder.u32()?;
        let chain_id = decoder.u64()?;
        let genesis_hash = decoder.b256()?;
        let commitment_scheme_version = decoder.u32()?;
        let topology_len = decoder.u16()?;
        let topology = decoder.take(usize::from(topology_len))?.to_vec();
        crate::CeTopologyV1::decode(&topology)
            .map_err(|_| PersistenceError::InvalidTopologyIdentity)?;
        let tree_format = decoder.string_u16()?;
        let vendor_revision = decoder.string_u16()?;
        decoder.finish()?;
        Ok(Self {
            local_storage_schema_version,
            chain_id,
            genesis_hash,
            commitment_scheme_version,
            topology,
            tree_format,
            vendor_revision,
        })
    }
}

pub(super) fn validate_expected_environment_identity(
    expected_identity: &EnvironmentIdentity,
) -> Result<(), PersistenceError> {
    if expected_identity.local_storage_schema_version != LOCAL_STORAGE_SCHEMA_VERSION {
        return Err(PersistenceError::UnsupportedLocalSchema {
            actual: expected_identity.local_storage_schema_version,
        });
    }
    if expected_identity.tree_format.is_empty() || expected_identity.vendor_revision.is_empty() {
        return Err(PersistenceError::EmptyEnvironmentIdentityField);
    }
    crate::CeTopologyV1::decode(&expected_identity.topology)
        .map_err(|_| PersistenceError::InvalidTopologyIdentity)?;
    Ok(())
}
