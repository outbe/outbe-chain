use alloy_primitives::{Address, Signature, B256};
use outbe_tee::lifecycle::{enrollment::EnrollmentSignerV1, renewal::LifecycleSignerV1};
use std::sync::Arc;

pub(super) struct RuntimeSigner {
    pub(super) operator: Arc<outbe_evm::OutbeEvmSigner>,
    pub(super) node: k256::ecdsa::SigningKey,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum SigningError {
    #[error("operator signing failed")]
    Operator(#[from] outbe_primitives::signer::SignerError),
    #[error("NodeHost signing failed")]
    Node(#[from] k256::ecdsa::Error),
}

impl EnrollmentSignerV1 for RuntimeSigner {
    type Error = SigningError;
    fn sign_operator_hash(&self, hash: B256) -> Result<Signature, Self::Error> {
        let bytes = self.operator.sign_hash(&hash)?;
        Ok(Signature::from_bytes_and_parity(&bytes[..64], bytes[64] != 0).normalized_s())
    }
    fn sign_node_hash(&self, hash: B256) -> Result<[u8; 65], Self::Error> {
        let (signature, recovery) = self.node.sign_prehash_recoverable(hash.as_slice())?;
        let mut bytes = [0; 65];
        bytes[..64].copy_from_slice(&signature.to_bytes());
        bytes[64] = recovery.to_byte();
        Ok(bytes)
    }
}
impl LifecycleSignerV1 for RuntimeSigner {
    type Error = SigningError;
    fn address(&self) -> Address {
        self.operator.address()
    }
    fn sign_node_hash(&self, hash: B256) -> Result<[u8; 65], Self::Error> {
        EnrollmentSignerV1::sign_node_hash(self, hash)
    }
    fn sign_transaction_hash(&self, hash: B256) -> Result<Signature, Self::Error> {
        self.sign_operator_hash(hash)
    }
}
