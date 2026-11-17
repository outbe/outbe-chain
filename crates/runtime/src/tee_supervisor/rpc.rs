//! Checkpoint-bound upstream lifecycle I/O, available before the local node starts.
use alloy_primitives::{Address, Bytes, B256, U256};
use alloy_sol_types::SolCall;
use jsonrpsee::{
    core::client::ClientT,
    http_client::{HttpClient, HttpClientBuilder},
    rpc_params,
};
use outbe_primitives::{addresses::TEE_REGISTRY_ADDRESS, validator_dto::ValidatorRecord};
use outbe_tee::{
    dcap_protocol::DcapOnboardingContextV1, lifecycle::*, registry_view::NodeBindingSelectorV1,
    upgrade_transfer::UpgradeKeyProofV1,
};
use serde_json::{json, Value};

mod history;
mod opening;
mod proof;
mod receipt;
mod registry;
mod send;
use send::RejectedNonce;
#[cfg(test)]
mod tests;

#[derive(Debug, thiserror::Error)]
pub(crate) enum RpcLifecycleError {
    #[error("upstream JSON-RPC request failed")]
    Rpc(#[from] jsonrpsee::core::client::Error),
    #[error("invalid certified RLP header")]
    Rlp(#[source] alloy_consensus::private::alloy_rlp::Error),
    #[error("invalid RPC JSON")]
    Json(#[from] serde_json::Error),
    #[error("invalid ABI response")]
    Abi(#[from] alloy_sol_types::Error),
    #[error("invalid hex response")]
    Hex(#[from] alloy_primitives::hex::FromHexError),
    #[error("invalid hexadecimal quantity")]
    Quantity(#[from] std::num::ParseIntError),
    #[error("RPC response is missing {0}")]
    Missing(&'static str),
    #[error("upstream network or checkpoint mismatch")]
    CheckpointMismatch,
    #[error("invalid block artifacts")]
    BlockArtifacts(#[from] outbe_primitives::error::PrecompileError),
    #[error("invalid lifecycle proof: {0}")]
    InvalidProof(ProofFailure),
    #[error("invalid TEE codec response")]
    Codec(#[from] outbe_primitives::tee_attestation_v1::CodecError),
    #[error("invalid finalized admission encoding")]
    Admission(#[from] outbe_tee::finalized_admission::FinalizedAdmissionCodecError),
    #[error("invalid upgrade transfer")]
    Transfer(#[from] outbe_tee::errors::TransportError),
    #[error("invalid DCAP artifact: {0:?}")]
    Dcap(outbe_tee::dcap_protocol::DcapRejectCodeV1),
    #[error("invalid registry view")]
    Registry(#[source] Box<outbe_tee::registry_view::RegistryError<RpcLifecycleError>>),
    #[error("invalid finality envelope")]
    Finality(#[source] Box<dyn std::error::Error + Send + Sync>),
    #[error("quantity exceeds supported width")]
    Overflow,
}

#[derive(Debug)]
pub(crate) enum ProofFailure {
    MissingAnchor,
    MissingAdmission,
    InvalidEnvelope,
    InvalidOpening,
    MissingArtifact,
    AmbiguousArtifact,
    ArtifactMismatch,
    InvalidValidator,
    InvalidReceipt,
}
impl std::fmt::Display for ProofFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl From<RpcLifecycleError> for LifecyclePortError<RpcLifecycleError> {
    fn from(source: RpcLifecycleError) -> Self {
        Self::Source(source)
    }
}

pub(crate) struct RpcLifecyclePort {
    client: HttpClient,
    chain_id: u64,
    genesis_hash: B256,
}
impl RpcLifecyclePort {
    pub(crate) fn new(
        upstream: &str,
        chain_id: u64,
        genesis_hash: B256,
    ) -> Result<Self, RpcLifecycleError> {
        let url = if upstream.contains("://") {
            upstream.to_owned()
        } else {
            format!("http://{upstream}")
        };
        let limits = outbe_consensus::follow_transport::UpstreamLimits::DEFAULT;
        let client = HttpClientBuilder::default()
            .request_timeout(limits.request_timeout)
            .max_response_size(limits.max_response_bytes)
            .build(url)?;
        Ok(Self {
            client,
            chain_id,
            genesis_hash,
        })
    }
    fn tag(at: LifecycleCheckpointV1) -> Value {
        json!({"blockHash":at.hash,"requireCanonical":true})
    }
    async fn block(&self, tag: Value) -> Result<Value, RpcLifecycleError> {
        Ok(self
            .client
            .request("eth_getBlockByNumber", rpc_params![tag, false])
            .await?)
    }
    async fn check(&self, at: LifecycleCheckpointV1) -> Result<(), RpcLifecycleError> {
        if at.chain_id != self.chain_id || at.genesis_hash != self.genesis_hash {
            return Err(RpcLifecycleError::CheckpointMismatch);
        }
        let chain: String = self.client.request("eth_chainId", rpc_params![]).await?;
        let genesis = self.block(json!("0x0")).await?;
        let canonical = self.block(json!(format!("0x{:x}", at.height))).await?;
        if quantity(&chain)? != self.chain_id
            || hash(&genesis, "hash")? != self.genesis_hash
            || self.checkpoint(&canonical)? != at
        {
            return Err(RpcLifecycleError::CheckpointMismatch);
        }
        let finalized = self.block(json!("finalized")).await?;
        if number(&finalized, "number")? < at.height {
            return Err(RpcLifecycleError::CheckpointMismatch);
        }
        Ok(())
    }
    fn checkpoint(&self, block: &Value) -> Result<LifecycleCheckpointV1, RpcLifecycleError> {
        Ok(LifecycleCheckpointV1 {
            height: number(block, "number")?,
            hash: hash(block, "hash")?,
            state_root: hash(block, "stateRoot")?,
            timestamp: number(block, "timestamp")?,
            base_fee_per_gas: word(block, "baseFeePerGas")?,
            chain_id: self.chain_id,
            genesis_hash: self.genesis_hash,
        })
    }
    async fn call(
        &self,
        at: LifecycleCheckpointV1,
        to: Address,
        data: &[u8],
    ) -> Result<Bytes, RpcLifecycleError> {
        Ok(self
            .client
            .request(
                "eth_call",
                rpc_params![
                    json!({"to":to,"data":Bytes::copy_from_slice(data)}),
                    Self::tag(at)
                ],
            )
            .await?)
    }
    async fn scalar<C: SolCall>(
        &self,
        at: LifecycleCheckpointV1,
        call: C,
    ) -> Result<u64, RpcLifecycleError> {
        use alloy_sol_types::SolValue;
        let data = self
            .call(at, TEE_REGISTRY_ADDRESS, &call.abi_encode())
            .await?;
        let value = U256::abi_decode(&data)?;
        u64::try_from(value).map_err(|_| RpcLifecycleError::Overflow)
    }
}
fn field<'a>(value: &'a Value, name: &'static str) -> Result<&'a str, RpcLifecycleError> {
    value
        .get(name)
        .and_then(Value::as_str)
        .ok_or(RpcLifecycleError::Missing(name))
}
fn quantity(value: &str) -> Result<u64, RpcLifecycleError> {
    Ok(u64::from_str_radix(
        value
            .strip_prefix("0x")
            .ok_or(RpcLifecycleError::Missing("hex prefix"))?,
        16,
    )?)
}
fn number(value: &Value, name: &'static str) -> Result<u64, RpcLifecycleError> {
    quantity(field(value, name)?)
}
fn hash(value: &Value, name: &'static str) -> Result<B256, RpcLifecycleError> {
    Ok(field(value, name)?.parse()?)
}
fn word(value: &Value, name: &'static str) -> Result<U256, RpcLifecycleError> {
    Ok(serde_json::from_value(
        value
            .get(name)
            .cloned()
            .ok_or(RpcLifecycleError::Missing(name))?,
    )?)
}
fn bytes(value: &Value, name: &'static str) -> Result<Vec<u8>, RpcLifecycleError> {
    Ok(alloy_primitives::hex::decode(field(value, name)?)?)
}

impl LifecycleChainPort for RpcLifecyclePort {
    type Error = RpcLifecycleError;
    async fn finalized_checkpoint(
        &self,
    ) -> Result<LifecycleCheckpointV1, LifecyclePortError<Self::Error>> {
        let at = self.checkpoint(&self.block(json!("finalized")).await?)?;
        self.check(at).await?;
        Ok(at)
    }
    async fn registry_state(
        &self,
        at: LifecycleCheckpointV1,
        selector: &NodeBindingSelectorV1,
    ) -> Result<LifecycleRegistryStateV1, LifecyclePortError<Self::Error>> {
        Ok(self.registry(at, selector).await?)
    }
    async fn pending_upgrade_nonce(
        &self,
        at: LifecycleCheckpointV1,
        selector: &NodeBindingSelectorV1,
    ) -> Result<u64, LifecyclePortError<Self::Error>> {
        self.check(at).await?;
        let pending = self.pending(at, selector).await?;
        self.check(at).await?;
        Ok(pending.nonce)
    }
    async fn validator_record(
        &self,
        at: LifecycleCheckpointV1,
        validator: Address,
    ) -> Result<Option<ValidatorRecord>, LifecyclePortError<Self::Error>> {
        Ok(self.validator(at, validator).await?)
    }
    async fn balance(
        &self,
        at: LifecycleCheckpointV1,
        account: Address,
    ) -> Result<U256, LifecyclePortError<Self::Error>> {
        self.check(at).await?;
        let value = self
            .client
            .request("eth_getBalance", rpc_params![account, Self::tag(at)])
            .await
            .map_err(RpcLifecycleError::from)?;
        self.check(at).await?;
        Ok(value)
    }
    async fn account_nonce(
        &self,
        account: Address,
        tag: LifecycleNonceTagV1,
    ) -> Result<u64, LifecyclePortError<Self::Error>> {
        let block = match tag {
            LifecycleNonceTagV1::Finalized(at) => {
                self.check(at).await?;
                Self::tag(at)
            }
            LifecycleNonceTagV1::Latest => json!("latest"),
            LifecycleNonceTagV1::Pending => json!("pending"),
        };
        let value: String = self
            .client
            .request("eth_getTransactionCount", rpc_params![account, block])
            .await
            .map_err(RpcLifecycleError::from)?;
        if let LifecycleNonceTagV1::Finalized(at) = tag {
            self.check(at).await?;
        }
        Ok(quantity(&value)?)
    }
    async fn send_raw_transaction(
        &self,
        raw_signed_tx: &[u8],
    ) -> Result<B256, LifecyclePortError<Self::Error>> {
        match self
            .client
            .request(
                "eth_sendRawTransaction",
                rpc_params![Bytes::copy_from_slice(raw_signed_tx)],
            )
            .await
        {
            Ok(hash) => Ok(hash),
            Err(error) => {
                let classification = if matches!(&error, jsonrpsee::core::client::Error::Call(_)) {
                    self.rejected_nonce(raw_signed_tx).await
                } else {
                    None
                };
                let source = RpcLifecycleError::Rpc(error);
                match classification {
                    Some(RejectedNonce::Consumed) => {
                        Err(LifecyclePortError::NonceTooLow { source })
                    }
                    Some(RejectedNonce::Occupied(nonce)) => {
                        Err(LifecyclePortError::TransactionDisplaced { nonce, source })
                    }
                    None => Err(LifecyclePortError::Source(source)),
                }
            }
        }
    }
    async fn finalized_receipt(
        &self,
        at: LifecycleCheckpointV1,
        tx_hash: B256,
    ) -> Result<Option<LifecycleReceiptV1>, LifecyclePortError<Self::Error>> {
        Ok(self.receipt(at, tx_hash).await?)
    }
    async fn onboarding_proof(
        &self,
        at: LifecycleCheckpointV1,
        intent_hash: B256,
    ) -> Result<LifecycleOnboardingProofV1, LifecyclePortError<Self::Error>> {
        Ok(self.onboarding(at, intent_hash).await?)
    }
    async fn upgrade_key(
        &self,
        at: LifecycleCheckpointV1,
        context: &DcapOnboardingContextV1,
        proof: &UpgradeKeyProofV1,
    ) -> Result<LifecycleOnboardingProofV1, LifecyclePortError<Self::Error>> {
        self.check(at).await?;
        self.context_identity(context)?;
        proof.validate().map_err(RpcLifecycleError::from)?;
        self.proof_checkpoint(at, proof)?;
        let artifact: Bytes = self
            .client
            .request(
                "outbe_upgradeKeyV1",
                rpc_params![Bytes::from(context.encode_canonical()), proof, false],
            )
            .await
            .map_err(RpcLifecycleError::from)?;
        let decoded =
            outbe_tee::dcap_protocol::DcapOnboardingArtifactV1::decode_canonical(&artifact)
                .map_err(RpcLifecycleError::Dcap)?;
        if decoded.context != *context {
            return Err(RpcLifecycleError::InvalidProof(ProofFailure::ArtifactMismatch).into());
        }
        self.check(at).await?;
        Ok(LifecycleOnboardingProofV1 {
            artifact,
            proof: proof.clone(),
        })
    }
}
