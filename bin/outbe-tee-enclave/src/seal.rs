//! Enclave boot paths for sealed-blob storage, re-exporting pure `TSEAL` crypto
//! from [`outbe_enclave::seal`].

use std::path::PathBuf;

use alloy_primitives::B256;

pub use outbe_enclave::seal::{
    seal_tribute_offer_and_group_sig, unseal_network_bound_payload,
    unseal_tribute_offer_and_group_sig, KeyPolicy, SealHeader, UnsealedTributeOfferAndGroupSig,
    HEADER_LEN, NETWORK_BINDING_LEN, SEAL_FORMAT, SEAL_MAGIC,
};

/// Boot-time configuration that the sealing path needs. Startup builds it once
/// from CLI args, and the seal-on-bootstrap / unseal-on-restart path consumes it.
#[derive(Clone, Debug)]
pub struct EnclaveBootConfig {
    pub chain_id: B256,
    pub tee_dir: PathBuf,
    pub isv_svn: u16,
}

impl EnclaveBootConfig {
    /// Build from a raw 32-byte chain id, the tee directory, and the running SVN.
    pub fn new(chain_id: [u8; 32], tee_dir: PathBuf, isv_svn: u16) -> Self {
        Self {
            chain_id: B256::from(chain_id),
            tee_dir,
            isv_svn,
        }
    }

    /// Path to the sealed tribute-offer-key blob under the tee dir
    /// (`<tee-dir>/sealed_root.bin`).
    pub fn sealed_root_path(&self) -> PathBuf {
        self.tee_dir.join("sealed_root.bin")
    }

    pub fn sealed_identity_path(&self) -> PathBuf {
        self.tee_dir.join("sealed_identity.bin")
    }

    /// Write-once sealed NodeHost authorization installed by I2 initialization.
    pub fn sealed_node_authorization_path(&self) -> PathBuf {
        self.tee_dir.join("sealed_node_authorization_v1.bin")
    }
}

/// Fixed mock sealing key. It is stable across rebuilds so it simulates MRSIGNER
/// (a rebuilt mock enclave unseals an older mock blob). Gated so it never links
/// into the production binary.
#[cfg(any(test, feature = "mock"))]
pub const MOCK_SEALING_KEY: [u8; 32] = [0x42; 32];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boot_config_new_and_sealed_path() {
        let cfg = EnclaveBootConfig::new([0xCD; 32], PathBuf::from("/var/lib/outbe/tee"), 7);
        assert_eq!(cfg.isv_svn, 7);
        assert_eq!(
            cfg.sealed_root_path(),
            PathBuf::from("/var/lib/outbe/tee/sealed_root.bin")
        );
    }
}
